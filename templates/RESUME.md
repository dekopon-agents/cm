> **Nothing is running yet.** Fill this banner at every handoff: what is running, what to read next, and the `/loop` command that resumes it.
>
> ```
> /loop You are the campaign coordinator for <unit> (<scope>). Read RESUME.md, OWNER-QUEUE.md, <NN-name>/<unit>/STATE.md and LIMITS.toml, and follow the campaign skill. Drive <unit> to <end state>. No check-ins: decide under ambiguity and journal it. Wake only on completion notices. Write state before every wait.
> ```

# Resume

## State today

Nothing has shipped yet.

## How to work

Follow the campaign skill for roles, launches, the coordinator loop, landing, closing and spend. State changes go through cm: `cm journal`, `cm launch` and `cm advance`. `cm lint` lists every hand edit to a file cm owns.

## Read, in this order

1. The banner above.
2. The campaign skill.
3. `LIMITS.toml`, then `OWNER-QUEUE.md`.
4. The sub-campaign's `BRIEF.md` and `DECISIONS.md`, then the unit's `STATE.md`.
