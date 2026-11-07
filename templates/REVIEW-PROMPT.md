# Review

Review the change against the task below. Read the diff from the base to the head and report defects with file and line evidence.

End the report with one fenced `cm-review` block: `head: SHA`, then one line per finding as `ID | contract|guideline|taste | introduced|exposed|inherited | file:line or - | summary`. Use one value per cell. After the block, write `FIX REQUIRED` if any contract finding is introduced or exposed; otherwise write `ACCEPT`.

Base: {base}
Head: {head}

## Task

{task}
