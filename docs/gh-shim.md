# gh routing shim: declared rows

The shim classifies each `gh` invocation against the signed
`gh-routing-manifest` and routes it by authority class. A row is only live when
BOTH the signed manifest declares it AND this build's classifier allowlist
admits it at that manifest version, so a signed artifact alone cannot widen what
the shim will speak.

## Authority classes

| Class | Tier | Identity the call runs under | Behavior |
| --- | --- | --- | --- |
| MECHANICAL | `mechanical` | none (upstream `gh`) | replaced by upstream `gh`; the shim holds no token |
| SPEECH-AS-BOT | `governed` | the seat's bot App | canonicalized into a structured request and routed to the holder |
| ADMINISTRATION | `admin` | the operator | refused unless `GH_SHIM_BYPASS=operator`, which records an operator-attributed audit line |

Anything not named below is `gh_shim_unclassified` (exit 86, nothing sent). The
refusal names the verb the classifier decided on, because the decision is made
on the verb alone.

## Commands on repositories no bot is bound to

A command aimed at a repository the signed manifest binds no bot to cannot be
bot speech, and upstream `gh` would run it under the operator's own login.
Unless it is known to be safe (below), such a command is refused with
`gh_shim_unbound_target` (exit 86, nothing sent) unless
`GH_SHIM_BYPASS=operator` is set. For example:

```text
gh-shim: gh_shim_unbound_target: `issue comment` targets earendil-works/pi, which is not a bot-bound repository (the signed gh routing manifest binds no bot to it); bot speech is not possible there, and upstream gh would run it under the operator's own login. The operator can approve it: re-run with GH_SHIM_BYPASS=operator, and the shim records an operator-attributed audit line.
```

With the bypass the command runs under the operator's own `gh`. Before starting
it, the shim appends and syncs an audit line in the same format the bypass
writes for `pr merge` and `release create`: `{as_of_unix_secs, tuple,
repository}`, where `tuple` is the verb (`repo create`) or
`api:<METHOD>:<endpoint>` and `repository` is `null` when none is known.

**What passes through.** The check is a safe list, not a list of writes: a
verb or subcommand the shim does not recognise (`codespace create`, or one a
future `gh` adds, even under a listed verb such as `config`) is refused like
any other write. Every entry is an exact subcommand. Only these pass through
unapproved:

- reads: `issue`/`pr`/`release`/`repo`/`run`/`workflow`/`label`/`cache`
  views, lists and downloads, `search` `issues`/`prs`/`repos`/`code`/`commits`,
  `status`, `org list`, `gist` `list`/`view`, `secret list`, `variable`
  `list`/`get`, `ruleset` `list`/`view`/`check`, `project`
  `list`/`view`/`field-list`/`item-list`, `ssh-key list`, `gpg-key list`,
  `codespace list`, `release verify`/`verify-asset`, `attestation verify`,
  `extension list`/`search`, and the `list`/`view` actions of
  `repo deploy-key` and `repo autolink`;
- commands that act only on the local machine: `auth status` (without a token
  flag), `config` `get`/`set`/`list`/`clear-cache`, `alias`
  `list`/`set`/`delete`/`import` (managing aliases), `completion` for bash,
  zsh, fish or powershell, `help` alone or for a gh command or help topic,
  `version`, `repo clone`, `gist clone`, `pr checkout`, and
  `browse --no-browser` with at most one location argument;
- anything with `--help`, and `gh` with no verb;
- `gh api` reads: GET or HEAD (a method named with `--method`/`-X`, else POST
  once a field or `--input` is given, as upstream `gh` decides), and a
  `gh api graphql` call whose inline `query` holds no `mutation`.

A flag before the verb (`gh --something search issues`) is one the shim does
not model, so it cannot tell which verb upstream `gh` would run; the command
is refused. A value flag the shim reads as a subcommand (`gh status -o org`)
is refused the same way.

Running an extension (`gh extension exec`, or `gh <extension>`) or an alias
(`gh <alias>`) is not on the list: an extension is a program and an alias can
expand to `api --method POST` or a shell command, so either can write with the
operator's token where the shim cannot see it.

## The operator's gh credentials

`gh auth token` and `gh auth status --show-token` (or `-t`) print the
operator's GitHub token into the agent's session, and with the token an agent
could call the GitHub API directly, around the shim. `gh auth login`,
`logout`, `refresh`, `switch` and `setup-git`, and any other `auth`
subcommand, change the operator's gh login or git's credential configuration.
With a verified manifest all of these are refused with
`gh_shim_operator_credentials` (exit 86, nothing sent) wherever they run, in a
bound checkout or not, unless `GH_SHIM_BYPASS=operator` is set:

```text
gh-shim: gh_shim_operator_credentials: `auth token` prints the operator's GitHub token into this agent's session, and with it an agent could call the GitHub API directly, around the shim. The operator can approve it: re-run with GH_SHIM_BYPASS=operator, and the shim records an operator-attributed audit line.
```

With the bypass the command runs after an audit line
`{as_of_unix_secs, tuple: "auth token", repository: null}`. `gh auth status`
without a token flag only reports which account is logged in and passes
through.

**What the target is.** The same resolver the governed rows use: `--repo`/`-R`,
a thread URL, a `gh repo` subcommand's repository positional (`gh repo delete
owner/name`), a `/repos/<owner>/<name>` endpoint, `GH_REPO`, then the working
directory's `origin`. When that target is bound, the governed path applies
unchanged, and the bypass there still reaches only manifest-declared
administration. A command that is not bot speech, run from inside a bound
checkout, also stays on the governed path whatever it names, as before.

**Account writes** never have a binding, so they need the bypass even from a
bound checkout: `repo create` (a new repository cannot be bound yet),
`repo fork`, `gist` writes, `ssh-key`/`gpg-key` add and delete, `project`
writes, and `gh api` writes to an endpoint outside `/repos/<owner>/<name>`
(such as `POST /user/repos` or a GraphQL mutation).

**No determinable target.** A command not on the safe list whose target cannot
be resolved (nothing named and no github.com `origin`, or a `--repo` that is
not a github.com `owner/name`) is refused the same way, and the refusal says
why.

The check needs a verified signed manifest to compare against. Without one (no
manifest installed, or one that never verified) nothing is bound and the shim
passes commands through as before. An installed manifest that fails validation
after an earlier one verified (a regressed manifest) already refuses every
command that is not a read. Destructive forms keep their own refusal, and
`github.shim: false` turns the whole shim off.

The operator's own terminal is not affected. The shim runs only where AFT puts
its `shims` directory first on `PATH`, which it does for the bash and PTY
children it spawns for agents; it never edits shell startup files or the
terminal's `PATH`, so a `gh` the operator types reaches upstream `gh` directly.

## Rows

| Row | Class | Since |
| --- | --- | --- |
| `issue view`, `issue list`, `pr view`, `pr list`, `pr diff`, `pr checks`, `run view`, `run list`, `repo clone`, `repo view` | MECHANICAL | v1 |
| `run watch`, `workflow view`, `workflow list` | MECHANICAL | v1 (classifier read-only set) |
| `api` GET `**` (field-free) | MECHANICAL | v1 |
| `issue comment`, `pr comment`, `pr review`, `issue reaction` | SPEECH-AS-BOT | v1 |
| `issue close`, `issue reopen`, `pr close`, `pr reopen` | SPEECH-AS-BOT | v12 |
| **`issue create`** | **SPEECH-AS-BOT** | **v14** |
| **`api` PATCH `/repos/*/*/issues/comments/*`** | **SPEECH-AS-BOT** | **v14** |
| `pr merge`, `release create` | ADMINISTRATION | v1 |
| `repo edit`, `run delete` | ADMINISTRATION | v9 |
| `workflow run`, `run rerun` | ADMINISTRATION | v10 |
| `run cancel` (including `--force`) | ADMINISTRATION-AS-OPERATOR | v15 |
| `release edit`, `release upload` | ADMINISTRATION | v13 |
| `api` PUT and DELETE `/repos/*/*/branches/*/protection` | ADMINISTRATION | v13 |
| **`issue edit`, label flags only, under `GH_SHIM_BYPASS=operator`** | **ADMINISTRATION (operator label row)** | **v14** |
| **`pr edit`, label flags only, under `GH_SHIM_BYPASS=operator`** | **ADMINISTRATION (operator label row)** | **v14** |
| **`label create`, under `GH_SHIM_BYPASS=operator`** | **ADMINISTRATION (operator label row)** | **v14** |
| `release delete`, `release delete-asset`, and any `release` verb with a `--delete-*` flag | refused as destructive | — |

`run cancel` uses the same operator gate as `run rerun`: without
`GH_SHIM_BYPASS=operator` it returns `gh_shim_admin_tier`; with the bypass
it appends and syncs the audit before upstream execution. Hung CI runs can
otherwise hold a seat until GitHub's timeout. `--force` is allowed because it
only changes the single selected run's cancellation endpoint to `force-cancel`,
not the scope of the operation (checked against gh 2.93.0 help and source).
Neither rerun nor cancel belongs in the unbound safe-read list: the existing
unbound-write refusal and audited bypass apply unchanged. The signed manifest
must declare cancellation at v15 or later on the bound-repository path; older
manifests retain their existing behavior. Code availability is not activation:
the credential-custody owner (CKCRED) signs the exact unsigned payload bytes for
an envelope-version-2 manifest, and the Subconscious daemon owner (SUBC)
coordinates placement. Production payloads and signing handoffs stay
outside git in `.alfonso/ceremonies/gh-routing-manifest-v15/`, never in public
docs or test fixtures. Prepare the payload from the trusted production v13
envelope with `python3 scripts/prepare-gh-shim-v15.py /path/to/v13-envelope.json`;
the script prints its SHA-256 and defaults to that private ceremony directory.
An optional second argument selects another output path. Verify the printed hash
before signing, and do not re-serialize the payload after signing.

Every row is declared for `macos` and `linux`: the schema requires a non-empty
`platform` list on tuples and API rules alike, so no row is OS-neutral.

## The v14 rows

Both are the same authority class as `issue comment` and `issue close`: public
speech under the bot identity that creates no authority and lands no code.

### `issue create`

Declared with the `fields-only` argv form, because a create names no target that
exists yet — the issue has no number until it exists, so the declaration carries
body fields and an empty target.

Admitted: `--title`, `--body`, `--body-file` (including the stdin spelling `-`),
`--label` once per label, and `--repo`. Repeated labels are collected into
GitHub's plural `labels` field.

Refused with `gh_shim_unsupported_flag` (exit 86, nothing sent, refused while
argv is read): `--assignee`, `--milestone`, `--project`, `--web`, `--template`,
`--recover`. Assignment, milestones and projects hand out work rather than
speak; the rest need an interactive terminal the governed seam cannot reproduce.
Short spellings of the refused flags are not admitted either — they refuse as
`gh_shim_unclassified`, also without sending anything.

A missing `--title` is upstream's error to report, not the shim's: `gh issue
create` already fails with its own text, and relaying that is more useful than a
second refusal invented here.

### `api` PATCH `/repos/*/*/issues/comments/*`

The id-addressed comment edit behind `edit(issue://N/comments/K)`, which refused
before v14 because no PATCH rule existed.

The endpoint carries the target (repository and comment id) and the row is
body-only: the shim parses the payload itself — a JSON object behind `--input`,
including the stdin spelling `-`, or a single `body` field — and forwards only
the field it recognized, rather than handing the holder bytes it never read. A
payload carrying anything besides `body` is not the declared request. Any other
flag is refused, because a governed route never runs upstream `gh` and silently
dropping a flag would change what the caller asked for.

Ownership is the route holder's check: the comment's author must be the calling
seat's bot. PATCH on any other path — including `/repos/*/*/issues/*`, the issue
itself — is not admitted by this row, and every other PATCH stays as v13 has it.

## Operator label rows (v14)

Maintainers running the shared design gate put `design-approved` on issues and
`trivial` on pull requests, and create those labels where a repository lacks
them. Labels are repository administration, not bot speech, so these rows run
under the operator's own `gh` with `GH_SHIM_BYPASS=operator`, like `pr merge`.
Each row is live only when the signed manifest declares its tuple at v14 or
later: `issue edit` in the governed tier, `pr edit` and `label create` in the
admin tier. Under the deployed v13 manifest none of them exists and the argv
stays `gh_shim_unclassified`.

Upstream `gh` runs the whole argv, so every argument outside a row refuses by
name (exit 86, nothing sent, no audit line), even beside an admitted flag: a
title, body or reviewer change riding along with a label would otherwise run
under the operator's identity without being recorded. The refusal names the
flag without echoing its value. The audit line is appended and synced before
upstream `gh` is spawned, so an attempt that dies mid-call is still on record.

### `issue edit` and `pr edit`, labels only

Admitted, and nothing else: `--add-label` and `--remove-label` (`--flag value`
or `--flag=value`, comma-separated labels, repeatable), `--repo`/`-R`, and
exactly one positional — an issue number or `https://github.com/<o>/<r>/issues/<n>`
URL for `issue edit`, a pull request number or
`https://github.com/<o>/<r>/pull/<n>` URL for `pr edit`. At least one label flag
is required. A branch name is not admitted for `pr edit`: the audit line records
the number that changed. `pr edit` works on any pull request, not only the
bot's own.

Audit line: `{as_of_unix_secs, tuple, repository, issue_number, labels_added,
labels_removed}` for `issue edit`, and the same with `pr_number` in place of
`issue_number` for `pr edit`.

Without the bypass `issue edit` stays on the governed own-issue route, and
`pr edit` has no bot-speech route: it refuses as `gh_shim_unclassified`, as it
did before v14.

### `label create`

Admitted, and nothing else (the flags `gh label create --help` lists): one
positional, the label name; `--color`/`-c` and `--description`/`-d` with a value;
`--force`/`-f`; `--repo`/`-R`. A second positional or any other flag refuses.

Audit line: `{as_of_unix_secs, tuple: "label create", repository, label, color}`;
`color` is `null` when none was given and upstream picks one.

Without the bypass it refuses as `gh_shim_unclassified`. `label delete` and
`label edit` stay undeclared.
