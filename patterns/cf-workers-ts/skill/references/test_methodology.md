# Test methodology for MCP servers

More real defects in the fleet this pattern came from were found by the practices below than by any
amount of reading. The recurring failure is not an untested path — it is a test
that passes while testing nothing.

## Route-level tests in the real runtime are not optional

`src/auth.ts` and `src/index.ts` import `cloudflare:` modules and are therefore
importable in **no** Node test. Before workerd-based route tests existed, the
helpers were well covered and the *wiring* was not: a regression that stopped
calling the state-validation function altogether failed no test. Reverting the
source made the Node suites fail to **resolve**, which is not coverage.

Two projects: `unit` for pure logic under Node, `workerd` via
`@cloudflare/vitest-pool-workers` for anything touching a `cloudflare:` import.
`SELF.fetch()` then exercises the deployed `wrangler.toml` bindings, vars and
compatibility flags.

## Mutation-test every security control

Break it, run the suite, require that a **named** test fails, restore. A control
whose removal breaks nothing is not protected, whatever the coverage number says.

Run it as a table, not ad hoc, and record the result next to the change. When a
control's expected failure count changes, find out why before accepting the new
number.

The rendered template's `README.md` carries that table for the template's own
controls: which source edit, and how many tests fail.

## Six ways a security test lies

Each of these was found by mutation, never by reading.

**Satisfied by the wrong refusal.** Guard tests calling a tool with `{}` arguments
hit *schema validation* before the authorization guard, and passed with the guard
deleted. Use valid arguments — always. The same shape appears with a deleted guard
making the tool *throw*: the SDK reports `isError`, and a test asserting "some
error" still passes. Pin the assertion to the **required scope name** or the
specific message.

**Unfalsifiable by construction.** An entire scope-enforcement layer was exported
and called from nowhere — the per-tool policy table was documentation. Separately,
two tests were written for a header-padding bypass that turned out not to be
reachable at all, because the Fetch spec normalizes header values on the way in.
Removing the code broke neither test. **If a mutation breaks nothing, the belief
behind the test is what to check first.**

**Tamper that tampered with nothing.** Flipping the last base64url character of a
256-byte RSA signature can leave the decoded bytes identical. Corrupt a middle
byte, and assert the string actually changed.

**Indistinguishable parties.** A replay test minted both tokens with the same
subject, so the caller binding under test could be deleted with every test green.
When a test is about two identities differing, make them differ.

**A negative control that failed to apply.** Twice: an edit landed on the doc
comment above a regex instead of the character class, and a patch script's
"already applied" guard short-circuited so the edit was made in memory and thrown
away. **Measure the control, don't reason about it** — assert that the mutation
changed the file.

**Coverage of one instance reading as coverage of a pattern.** Parameterizing a
guard test over `TOOL_SCOPES` is not enough if only one tool has a second scope to
step up to: the same test yielded 2 failures on one tool and 0 on two others.

## Structural limits worth stating rather than papering over

Some tests cannot exist on some servers, and writing them anyway produces exactly
the vacuity above.

- **A single-scope server** cannot express scope escalation, step-up, or a
  broader-scope consent re-prompt. Do not write a version that can never fail;
  put the test on a two-scope server and say why it lives there.
- **A one-tool server's** header/body mismatch test must name a **nonexistent**
  tool — comparing the only tool to itself is no mismatch, and the 200 is correct.
  Its deterministic-ordering test has to drop the length requirement.
- **Source-text guards are brittle by nature.** They pin literals in source, and a
  guard that fails when a formatter reflows the call trains people to delete
  guards. Write them reflow-tolerant, and delete them when the type system starts
  covering what they guarded.

## Two traps specific to this stack

**An empty test database reports as passing tests.** A workerd D1 with no schema
answers `no such table`, and a tool that catches its own errors turns that into a
tool-level error result. Every database-touching test in one repo had been doing
this — which is why inverting a destructive tool into refusing-by-default passed
211 tests. Create the schema in `beforeAll`, or commit migrations and apply them.

**A clean instantiation passing its suite is not a completeness check.** A
checklist-exact instantiation of the template passed its entire suite with placeholder
consent prose still in `SCOPE_HELP`, `database_name = "REPLACE"`, and
`src/skill.ts` untouched. The suite covers what the tests reference — scope names,
cookie prefixes, hostnames — and says nothing about prose or about config no test
reads. The checklist is the completeness check; the suite is not.

Related: **a README instruction can break CI.** `SCOPE_HELP` prose was asserted
literally by a test, so performing the one customization the README called out
failed the build — and `npm run ci` is the Workers Builds `build_command`. Export
the map and assert against it.

## Verify you are reading the repo you think you are

A deployed build's `commit_hash` must exist in the repo you are reading. Two
superseded local clones whose `wrangler.toml` names matched no deployed Worker
produced two wrong conclusions that were written into a plan and a skill before
anyone checked.

## Review

Any change touching auth, secrets, deploy config or data migrations gets a pass
from a **fresh** agent with no context from the work. Every such review of this
pattern's fleet found real defects the implementer had missed — including two authorization
holes and one latent privilege escalation that was still unreachable. Reviews by
the author found notes.
