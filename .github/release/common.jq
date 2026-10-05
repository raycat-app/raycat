# Общие определения для jq-программ выпуска. Коммит: {sha, subject, breaking, files, labels}.

def breaking:
  (.breaking // false) or (.subject | test("^[a-z]+(\\([^)]*\\))?!:"));

def kind:
  if breaking then "breaking"
  elif (.subject | test("^feat(\\([^)]*\\))?:")) then "feat"
  else "patch" end;

# Следующая stable-версия. Вход: массив коммитов с прошлой stable.
# До 1.0 несовместимое изменение поднимает minor, начиная с 1.0 — major.
def next_version($last):
  ($last | split(".") | map(tonumber)) as [$major, $minor, $patch]
  | (map(kind)) as $kinds
  | ($kinds | any(. == "breaking")) as $is_breaking
  | ($kinds | any(. == "feat")) as $is_feat
  | (if $is_breaking and $major >= 1 then [$major + 1, 0, 0]
     elif $is_breaking or $is_feat then [$major, $minor + 1, 0]
     else [$major, $minor, $patch + 1] end)
  | map(tostring) | join(".");
