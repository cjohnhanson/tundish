# Security policy

## Reporting

Do not open a public issue for a vulnerability. Report it privately:

**https://github.com/cjohnhanson/tundish/security/advisories/new**

That opens a thread only you and the maintainer can read.

Include what an attacker gains, what they must already control, the
affected commit, and steps that reproduce the problem.

## What happens next

tundish has one maintainer, so response is best effort. Expect a reply
within a week.

A confirmed report gets a fix and an advisory published together. You
are credited unless you ask otherwise.

## Scope

tundish stores JSON items in one local SQLite database. It opens no
network port and runs no server. Programs publish items through the CLI
or the Python API. A hook can pull rendered text into an agent's session.

tundish treats payloads as data and does not execute them. Subscription
filters and templates are jaq expressions that run in-process and see
only the item.

In scope:

- A payload causing code execution in tundish.
- A filter or template reading the environment, stdin, a file, or the
  network, or ending the process through a forbidden jaq function.
- Environment variables redirecting the default config lookup.
- A subscriber obtaining another subscriber's live lease or replacing
  its ack through the documented CLI rules.

Out of scope:

- A dependency advisory with no exploitable path through tundish.
  Report it to that dependency.
- Denial of service from a malformed local file where the caller already
  controls that file.

## Known boundaries

The database's permissions follow the user's umask. tundish does not
encrypt the database or enforce a separate user-access policy. Anyone
with access to the file can read or change its contents. Subscriber
names are labels supplied through `--as`; they are not authenticated
identities.

tundish removes jaq's `env`, `halt`, `halt_error`, `input`, and `inputs`
functions. An expression cannot read the environment or stdin, or end
the process through those functions. The jaq functions exposed by
tundish provide no file or network access. Expressions still run in the
tundish process; they do not have a separate resource sandbox.

The default config is `~/.config/tundish/config.yml`. Home comes from
the passwd database, and the path takes no environment override.
`--config` explicitly names another file. `--db` and the config's
`database:` key can name another database.

Rendered payload text remains publisher-supplied content. tundish does
not decide whether an agent should follow instructions inside it. The
hook and agent host control how that text enters the session.
