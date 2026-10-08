use std::path::Path;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, output] = args.as_slice() else {
        anyhow::bail!(
            "использование: update-ru-ipv4 <delegated-ripencc-extended-latest> <data/ru-ipv4.txt>"
        );
    };
    let summary = raycat_routing::update::run(Path::new(input), Path::new(output))?;
    println!("снимок {}, подсетей {}", summary.date, summary.prefixes);
    Ok(())
}
