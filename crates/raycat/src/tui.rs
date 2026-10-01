//! `raycat tui`: полноэкранный интерфейс поверх API демона. Он ничего не меняет
//! сам: закрепление и обновление подписок идут через те же вызовы, что и у
//! команд CLI. Настройки и каталог состояния не нужны, только путь сокета.
//!
//! Состояние (`app`) отделено от отрисовки (`view`) и от связи с демоном (`link`):
//! ожидание идёт в одном `select!` по клавишам, сообщениям связи и таймеру, поэтому
//! без событий интерфейс не потребляет процессор.

mod app;
mod canvas;
mod link;
mod view;

use std::io::{self, IsTerminal as _};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event as TermEvent};
use raycat_config::Env;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::{Notify, mpsc};
use tokio::time::{MissedTickBehavior, interval};

use self::app::{App, Effect, Msg};
use self::canvas::Palette;
use self::link::Timing;
use crate::client::Client;
use crate::paths;
use crate::term::Term;
use crate::util::now_unix;

const INPUT_POLL: Duration = Duration::from_millis(250);
const TICK: Duration = Duration::from_secs(1);
const QUEUE: usize = 64;

pub(crate) fn run(env: &Env) -> Result<()> {
    if !(io::stdin().is_terminal() && io::stdout().is_terminal()) {
        bail!(
            "tui работает только в интерактивном терминале; для скриптов есть команды status, nodes, update и events с ключом --json"
        );
    }
    let socket = paths::client_socket(env, paths::is_root())?;
    let palette = Palette::new(Term::detect(env).color());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("не удалось запустить среду выполнения")?;
    runtime.block_on(session(socket, palette))
}

/// Полноэкранный режим: терминал возвращается в обычное состояние при любом
/// выходе, включая ошибку; при панике это делает обработчик ratatui.
struct Screen {
    terminal: DefaultTerminal,
}

impl Screen {
    fn open() -> Result<Self> {
        let terminal = ratatui::try_init()
            .inspect_err(|_| {
                let _ = ratatui::try_restore();
            })
            .context("не удалось перейти в полноэкранный режим")?;
        Ok(Self { terminal })
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = ratatui::try_restore();
    }
}

/// Сигналы завершения: в режиме raw Ctrl+C приходит клавишей, а `kill` и закрытие
/// терминала — сигналами, и экран нужно вернуть и в этих случаях.
struct Signals {
    terminate: Signal,
    hangup: Signal,
    interrupt: Signal,
}

impl Signals {
    fn new() -> Result<Self> {
        let open = |kind| signal(kind).context("не удалось подписаться на сигналы завершения");
        Ok(Self {
            terminate: open(SignalKind::terminate())?,
            hangup: open(SignalKind::hangup())?,
            interrupt: open(SignalKind::interrupt())?,
        })
    }

    async fn recv(&mut self) {
        tokio::select! {
            _ = self.terminate.recv() => {}
            _ = self.hangup.recv() => {}
            _ = self.interrupt.recv() => {}
        }
    }
}

/// Читает события терминала в отдельном потоке: чтение блокирующее, а поток
/// просыпается раз в `INPUT_POLL`, чтобы заметить, что интерфейс закрылся.
fn spawn_input() -> Result<mpsc::Receiver<io::Result<TermEvent>>> {
    let (sender, receiver) = mpsc::channel(QUEUE);
    thread::Builder::new()
        .name("raycat-tui-input".to_owned())
        .spawn(move || read_input(&sender))
        .context("не удалось запустить чтение клавиатуры")?;
    Ok(receiver)
}

fn read_input(sender: &mpsc::Sender<io::Result<TermEvent>>) {
    loop {
        match event::poll(INPUT_POLL) {
            Ok(true) => {
                let item = event::read();
                let failed = item.is_err();
                if sender.blocking_send(item).is_err() || failed {
                    return;
                }
            }
            Ok(false) => {
                if sender.is_closed() {
                    return;
                }
            }
            Err(error) => {
                let _ = sender.blocking_send(Err(error));
                return;
            }
        }
    }
}

enum Wake {
    Input(Option<io::Result<TermEvent>>),
    Message(Option<Msg>),
    Tick,
    Quit,
}

/// Выполняет действия пользователя в отдельных задачах: обновление подписок
/// долгое, и интерфейс не должен ждать его.
struct Runner {
    client: Arc<Client>,
    inbox: mpsc::Sender<Msg>,
    refresh: Arc<Notify>,
}

impl Runner {
    fn dispatch(&self, effect: Effect) {
        let client = Arc::clone(&self.client);
        let inbox = self.inbox.clone();
        let refresh = Arc::clone(&self.refresh);
        tokio::spawn(async move {
            refresh.notify_one();
            let msg = link::perform(&client, effect).await;
            let _ = inbox.send(msg).await;
            refresh.notify_one();
        });
    }
}

async fn session(socket: PathBuf, palette: Palette) -> Result<()> {
    let client = Arc::new(Client::new(socket));
    let (inbox, mut messages) = mpsc::channel::<Msg>(QUEUE);
    let refresh = Arc::new(Notify::new());
    let mut signals = Signals::new()?;
    let mut keys = spawn_input()?;
    let mut screen = Screen::open()?;
    let _link = link::spawn(
        Arc::clone(&client),
        inbox.clone(),
        Arc::clone(&refresh),
        Timing::default(),
    );
    let runner = Runner {
        client,
        inbox,
        refresh,
    };
    let mut ticks = interval(TICK);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut app = App::new(now_unix());
    while !app.quit {
        screen
            .terminal
            .draw(|frame| view::draw(frame, &mut app, palette))
            .context("не удалось перерисовать экран")?;
        let wake = tokio::select! {
            input = keys.recv() => Wake::Input(input),
            msg = messages.recv() => Wake::Message(msg),
            _ = ticks.tick() => Wake::Tick,
            () = signals.recv() => Wake::Quit,
        };
        app.set_now(now_unix());
        match wake {
            Wake::Input(Some(Ok(event))) => {
                if let TermEvent::Key(key) = event
                    && let Some(effect) = app.handle_key(key)
                {
                    runner.dispatch(effect);
                }
            }
            Wake::Input(Some(Err(error))) => bail!("не удалось прочитать ввод: {error}"),
            Wake::Input(None) => bail!("чтение ввода остановилось"),
            Wake::Message(Some(msg)) => {
                app.apply(msg);
                while let Ok(more) = messages.try_recv() {
                    app.apply(more);
                }
            }
            Wake::Message(None) => bail!("связь с демоном остановилась"),
            Wake::Tick => {}
            Wake::Quit => app.quit = true,
        }
    }
    Ok(())
}
