include "common";

# Входные данные (см. plan.sh):
#   $commits — коммиты от прошлой stable до конца main, от старых к новым;
#   $builds  — dev-выпуски {tag, sha, published_at (секунды)};
#   $last, $now, $mode (auto|manual), $requested, $ignore_wait, $stop.

def files: .files // [];

def category:
  if breaking then "breaking"
  elif ((.labels // []) | any(. == "безопасность")) and (.subject | test("^fix(\\([^)]*\\))?:")) then "hotfix"
  elif (files | any(. == "versions.toml")) then "xray"
  elif ((files | length) > 0)
       and (files | all(startswith("crates/emulation/profiles/") or startswith("crates/emulation/captures/")))
  then "emulation"
  else "code" end;

def wait_seconds:
  if . == "hotfix" then 0
  elif . == "emulation" then 86400
  else 259200 end;

def hours: ((. / 3600) | ceil);

# Причины, по которым нельзя продвинуть сборку на коммите с номером $p.
# Исправление безопасности освобождает от срока все коммиты до него включительно: в stable
# уходит сборка целиком, пересобрать её без прежних изменений нельзя.
def blockers($entries; $p):
  ($entries | map(select(.i <= $p))) as $in
  | ([$in[] | select(.category == "hotfix") | .i] | max // -1) as $waived
  | ($commits[0:($p + 1)] | next_version($last)) as $version
  | (if $mode == "auto" then
       [$in[] | select(.category == "breaking") | "несовместимое изменение, только вручную: \(.subject)"]
       + (if ($version | split(".") | .[0]) != ($last | split(".") | .[0])
          then ["смена мажорной версии, только вручную: v\($version)"] else [] end)
     else [] end)
    + (if $ignore_wait then []
       else [$in[]
             | select(.i > $waived and ($now - .published) < .wait)
             | "срок не вышел, осталось \((.wait - ($now - .published)) | hours) ч: \(.subject)"] end);

($commits | map(.sha)) as $shas
| ([ $builds[] | . as $b | ($shas | index($b.sha)) as $pos | select($pos != null) | $b + {pos: $pos} ] | sort_by(.pos)) as $bs
| [ range(0; ($commits | length)) as $i
    | $commits[$i] as $c
    | ($c | category) as $category
    | ([ $bs[] | select(.pos >= $i) ] | first | .published_at) as $published
    | { i: $i, sha: $c.sha, subject: $c.subject, category: $category,
        wait: ($category | wait_seconds), published: $published } ] as $entries
| [ $bs[] | . as $b | { build: $b, blockers: blockers($entries; $b.pos) } ] as $evals
| (
    if $stop then
      {candidate: null, reason: "на открытом issue висит метка «стоп-релиз»"}
    elif ($commits | length) == 0 then
      {candidate: null, reason: "нет изменений после последней stable-версии"}
    elif ($bs | length) == 0 then
      {candidate: null, reason: "нет dev-сборок новее последней stable-версии"}
    elif $mode == "manual" then
      (
        ([ $evals[] | select(.build.tag == $requested) ] | first) as $e
        | if $e == null then
            {candidate: null, reason: "dev-сборка \($requested) не найдена среди сборок новее последней stable-версии"}
          elif ($e.blockers | length) > 0 then
            {candidate: null, reason: ("сборка не готова: " + ($e.blockers | join("; ")))}
          else
            {candidate: $e.build, reason: "ручной запуск"}
          end
      )
    else
      (
        ([ $evals[] | select((.blockers | length) == 0) ] | last) as $e
        | if $e == null then
            {candidate: null, reason: ("нет сборок, готовых к выпуску; у новейшей: " + ($evals | last | .blockers | join("; ")))}
          else
            {candidate: $e.build, reason: "новейшая сборка, все изменения которой выдержали срок"}
          end
      )
    end
  ) as $decision
| {
    promote: ($decision.candidate != null),
    reason: $decision.reason,
    tag: ($decision.candidate.tag // ""),
    sha: ($decision.candidate.sha // ""),
    version: (if $decision.candidate != null
              then ($commits[0:($decision.candidate.pos + 1)] | next_version($last))
              else "" end),
    entries: $entries
  }
