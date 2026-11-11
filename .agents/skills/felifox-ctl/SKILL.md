---
name: felifox-ctl
description: Drive the Felifox browser from the shell with felifox-ctl - list tabs, navigate, take a snapshot of the elements on a page, click, type, press keys, evaluate JavaScript and save screenshots. Use when a task needs the real Felifox window, for example to test a change in Felifox or to operate a page with the logged-in sessions of the user.
---

# felifox-ctl

`felifox-ctl` sends commands to a Felifox instance that runs with agent control. The commands operate the active tab.

## Start

**Caution:** agent control lets each process of the user operate the logged-in sessions. Do not start Felifox with it unless the user agrees.

```sh
cargo build                                # builds felifox and felifox-ctl
FELIFOX_AGENT=1 target/debug/felifox &     # opens the control socket
target/debug/felifox-ctl tabs              # checks the connection
```

If `felifox-ctl` prints "Felifox does not listen", Felifox does not run or it started without `FELIFOX_AGENT=1`.

## Procedure

1. Load the page: `felifox-ctl navigate <url>`.
2. Wait until `felifox-ctl tabs` shows `"loading": false` for the active tab.
3. Take a snapshot: `felifox-ctl snapshot`.
4. Use the references from the snapshot in `click` and `type`.
5. Take a new snapshot after each action that changes the page.

A snapshot has one line for each visible element that accepts input:

```
https://example.com/login | Sign in
@e0 link "Home"
@e1 textbox "Email" = ""
@e2 textbox "Password"
@e3 checkbox "Remember me" = false
@e4 button "Sign in"
```

The references are valid only until the next snapshot. A snapshot does not show password values.

## Commands

| Command | Result |
| --- | --- |
| `tabs` | Lists the tabs as JSON: index, title, URL, active, loading |
| `tab <index>` | Selects a tab |
| `navigate <url>` | Loads a URL in the active tab. Text that is not a URL starts a search. |
| `snapshot` | Lists the elements and gives new references |
| `click <@ref>` | Scrolls to the element and clicks it |
| `type <@ref> <text>` | Replaces the value of a field. For a select, give the option value or label. |
| `press <key>` | Sends a key to the focused element: `Enter`, `Tab`, `Escape`, `ArrowDown`, `a` |
| `eval <expression>` | Evaluates one JavaScript expression and prints the result as JSON |
| `screenshot [path]` | Saves a PNG of the visible page and prints the path |

The exit status is 0 on success and 1 on an error. The error text goes to stderr.

## Native mode

`click`, `type` and `press` send synthetic JavaScript events by default. Some sites ignore them. If an action has no effect, do it again with `--native`:

```sh
felifox-ctl --native click @e4
felifox-ctl --native type @e1 "text"
felifox-ctl --native press Enter
```

Native mode sends a real mouse or keyboard event to the page. Use it for:

- Controls that ignore a synthetic click
- Keys that need the default action of the browser, for example `Tab` or the arrow keys
- Inputs that do not accept a synthetic value

## Limits

- `eval` takes one expression and does not wait for a promise. For statements, use a function: `(() => { const x = 1; return x })()`.
- A snapshot includes the main document and open shadow roots. It does not include frames.
- Native keys accept the modifiers `Meta+`, `Alt+`, `Shift+` and `Control+` only with a named key, for example `Meta+Shift+ArrowLeft`. `Control+a` gives an error. Felifox must be the active app for a key with a modifier.
- A click on a link that opens a new window makes a new tab. Use `tabs` and `tab <index>` to find and select it.
- "The page gave no result" shows that the page did not load fully, or that the expression is not valid.
- "Unknown or stale reference" shows that the page changed. Take a new snapshot.
