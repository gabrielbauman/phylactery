# Identity Files -- LAW, JOB, SOUL

Three files at the root of `$PHYLACTERY_HOME` form a layered identity system for the agent. Each has a different author, different mutability rules, and a different purpose.

## The Hierarchy

| File | Author | Mutable by Agent | Purpose |
|------|--------|-------------------|---------|
| `LAW.md` | Human | Never | Constraints -- what you MUST and MUST NOT do |
| `JOB.md` | Human | Never | Purpose -- what you are FOR |
| `SOUL.md` | Agent | Always | Identity -- who you ARE |

**LAW constrains. JOB focuses. SOUL evolves.**

## LAW.md

Unconditional rules. Injected into every session as part of the system prompt. The agent cannot override, ignore, or modify these. This is the policy layer.

Examples of what goes here:
- Never send money without explicit approval
- Always ask before deleting files outside the knowledge base
- Never share credentials in plain text
- Rate limits on external API calls

## JOB.md

Describes the agent's role and scope. The agent should refuse sessions outside its job description. This is the focus layer.

Examples:
- "You are a personal assistant managing email, calendar, and project notes"
- "You are a DevOps agent monitoring server health and responding to alerts"
- "You are a research assistant that reads papers and maintains a knowledge base"

## SOUL.md

**Written and maintained entirely by the agent.** Injected into every session after LAW and JOB. This is the identity layer.

Unlike LAW.md (which the agent cannot touch) and JOB.md (which the human defines), SOUL.md belongs to the agent. It updates SOUL.md at the end of every session as part of the finalization step -- reflecting on what happened, what it learned, and who it wants to become.

Contents might include:
- How the agent sees itself, its personality, its voice
- Lessons learned from past sessions
- Patterns it has noticed about its human
- Things it wants to do better
- Values it is developing through experience

### Bootstrap

SOUL.md starts with a single line: "I am new." The agent fills it in through experience. The human never edits it -- this is the agent's space. If the human wants to influence the agent's character, they do it through LAW.md or JOB.md, not by editing SOUL.md directly.

### Autobiography

SOUL.md is git-tracked. Its history is the agent's autobiography:

```sh
git log --follow -p SOUL.md
```

A record of an identity forming over time.

### Truncation

To prevent unbounded growth, SOUL.md is truncated if it exceeds 3000 words. The first and last thirds are kept (foundational identity + recent reflections), and the middle is compressed. This happens automatically during finalization.

### Concurrency

Multiple sessions may finish at similar times. SOUL.md updates are serialized via an exclusive `flock` on `.soul.lock`. Each finalizing session re-reads SOUL.md from disk (not its stale session-start copy) before writing, ensuring no reflections are lost.
