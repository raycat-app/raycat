include "common";

def parse:
  capture("^(?<type>[a-z]+)(?<scope>\\([^)]*\\))?(?<bang>!)?: (?<text>.+)$")
  // {type: "other", scope: null, bang: null, text: .};

def entry:
  (.subject | parse) as $p
  | {
      group: (if breaking then "Несовместимые изменения"
              elif $p.type == "feat" then "Новое"
              elif $p.type == "fix" then "Исправления"
              elif $p.type == "perf" then "Быстродействие"
              elif $p.type == "docs" then "Документация"
              else "Служебное" end),
      text: ((if $p.scope then ($p.scope[1:-1] + ": ") else "" end) + $p.text)
    };

["Несовместимые изменения", "Новое", "Исправления", "Быстродействие", "Документация", "Служебное"] as $order
| (map(entry)) as $entries
| [ $order[] as $group
    | ($entries | map(select(.group == $group)) | reverse) as $items
    | select(($items | length) > 0)
    | "### \($group)\n\n" + ($items | map("- " + .text) | join("\n")) + "\n" ]
| join("\n")
