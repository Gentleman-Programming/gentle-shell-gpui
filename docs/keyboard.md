# Keyboard reference

Everything the app does from the keyboard. In the app, **⌘/** (Ctrl+Shift+/ on Windows and Linux; also ⌃Space `?`, the command palette's "Keyboard shortcuts" and the app menu) opens the same list as a searchable screen. That screen is generated from the live keymap, so it is always right; this page is the written version.

Notation: ⌘ is Cmd on macOS and Ctrl elsewhere unless a column says otherwise. Off macOS the global chords add Shift or Alt where a plain Ctrl+letter belongs to terminals (Ctrl+L clears the screen, Ctrl+D ends input). The few plain Ctrl chords that remain (Ctrl+T, Ctrl+W, Ctrl+B, Ctrl+K, Ctrl+O, Ctrl+Q, Ctrl+[ and Ctrl+]) go to the terminal when a terminal pane has focus; see [Terminals](#terminals).

## Three layers

1. **Direct chords**: one chord, one action. Always available.
2. **Leader** (⌃Space, then one key): short, mnemonic, no modifiers to hold. The tmux/herdr style.
3. **Pane mode** (⌃Space, `Space`): sticky, so you can move, resize and split with bare keys until Esc.

While a sequence is pending (after ⌃Space, or the first `g` of `gg`), a which-key panel at the bottom of the window lists the keys that can come next and what they do. The leader guide belongs to the window, not to the view that started it: press ⌃Space in a terminal pane, the composer, Files, Changes, the tab sidebar or the canvas and it waits without a timeout until you press one of its keys or Esc. Choosing dispatches the command as if it had been pressed where you were; Esc gives focus back to that view exactly as it was (the terminal keeps its output, the composer its draft), and so does a click elsewhere. Only the leader is held this way: `gg` and the other list sequences keep the usual pending-input behavior, ending after about a second without a key. Pane mode shows its own keys in the same panel, and a PANE badge in the title bar.

## Direct chords

| Action | macOS | Windows / Linux |
| --- | --- | --- |
| New agent tab | ⌘T | Ctrl+T |
| New terminal tab | ⌘⇧T | Ctrl+Shift+T |
| New canvas tab | ⌘⇧E | Ctrl+Alt+Shift+E |
| Close pane (the last one closes the tab) | ⌘W | Ctrl+W |
| Close tab | ⌘⇧W | Ctrl+Shift+W |
| Reopen closed pane or tab | ⌥⌘T | Ctrl+Alt+Shift+T |
| Go to tab 1 … 8, 9 = last tab | ⌘1 … ⌘9 | Ctrl+1 … Ctrl+9 |
| Next / previous tab | ⌃Tab / ⌃⇧Tab | Ctrl+Tab / Ctrl+Shift+Tab |
| New pane (tiled) | ⌘D | Ctrl+Shift+D |
| Split down | ⌘⇧D | Ctrl+Shift+E |
| Next / previous pane | ⌘] / ⌘[ | Ctrl+] / Ctrl+[, or F6 / Shift+F6 |
| Focus the composer | ⌘L | Ctrl+Shift+L |
| Focus Files | ⌘⇧B | Ctrl+Shift+B |
| Focus Git changes | ⌘⇧G | Ctrl+Shift+G |
| Toggle the Files sidebar | ⌘B | Ctrl+B |
| Command palette | ⌘K | Ctrl+K |
| Browse folders (folder explorer; also picks the folder for Create workspace) | ⌘O | Ctrl+O |
| Workspaces | ⌃⌘K | Ctrl+Alt+K |
| Sessions | ⌘⇧O | Ctrl+Shift+O |
| Settings | ⌘, | Ctrl+, |
| **Keyboard shortcuts** | ⌘/ | Ctrl+Shift+/ |
| Zoom in / out / actual size | ⌘= ⌘- ⌘0 | Ctrl+= Ctrl+- Ctrl+0 |
| Quit | ⌘Q | Ctrl+Q |
| New session (in an agent pane) | ⌘N | Ctrl+N |
| Agents panel (in an agent pane) | ⌘⇧A | Ctrl+Shift+A |
| Stop the agent | ⌘. or Esc | Ctrl+. or Esc |

Esc closes the screen that is open; with none open, in an agent pane, it interrupts the turn.

## Leader: ⌃Space, then one key

⌃Space is the same on every platform. Press it, then one of:

| Key | Action |
| --- | --- |
| `h` `j` `k` `l` | Focus the pane to the left / below / above / right (spatial) |
| `v` / `s` | Split right / split down |
| `x` | Close pane |
| `u` | Reopen the last closed pane or tab |
| `c` / `t` / `Shift+c` | New agent tab / new terminal tab / new canvas tab |
| `n` / `p` | Next / previous tab |
| `1` … `9` | Go to tab N (9 = last tab) |
| `f` / `g` | Focus Files / focus Git changes |
| `i` | Focus the composer |
| `b` | Toggle the Files sidebar |
| `w` | Browse tabs in the sidebar (all workspaces; Enter opens, Esc returns) |
| `Shift+w` | Switch workspace… |
| `a` | Create workspace (choose the folder in the explorer) |
| `Shift+d` | Delete workspace… |
| `:` | Command palette |
| `?` | Keyboard shortcuts |
| `Space` | Enter pane mode |

## Pane mode

⌃Space then `Space` turns it on. It stays on until Esc or Enter, and while it is on every other key is swallowed, so you cannot type into a pane by accident. It also ends if a screen opens or the window loses focus.

| Key | Action |
| --- | --- |
| `h` `j` `k` `l` | Move focus left / down / up / right |
| `H` `J` `K` `L` (Shift) | Resize: move the focused pane's edge that way |
| `v` / `s` | Split right / split down |
| `x` | Close pane |
| `Esc` / `Enter` | Leave pane mode |

The palette also has Focus pane left/right/up/down, Split left/up and Resize pane left/right/up/down, each with its key.

## Files and Git changes

Click or focus a list (⌘⇧B for Files, ⌘⇧G for Changes, or ⌃Space `f` / `g`). Arrow keys and Enter always work. The letter keys below are **off by default**: turn on **Configuration → Appearance → Keyboard → Vim keys** (saved as `vimKeys` in the app's `settings.json`, effective at once). They never fire while a text field, such as the rename or commit box, has focus.

| Key | Files | Git changes | Needs Vim keys |
| --- | --- | --- | --- |
| ↑ ↓ | Previous / next | Previous / next | no |
| ← → | Collapse / expand | | no |
| ↵ | Open | Open the diff | no |
| Space | | Stage or unstage the selected file | no |
| `j` `k` | Next / previous | Next / previous | yes |
| `g g` / `G` | First / last | First / last | yes |
| `h` `l` | Collapse / expand | | yes |
| `o` | Open | | yes |
| `s` / `u` | | Stage / unstage | yes |
| `d` | | Open the diff | yes |

More Files keys (always on): open to the side ⌘↵, open in a new tab ⌥⌘↵, insert path in the composer ⌥↵, copy path ⌥⌘C, copy relative path ⌥⇧⌘C, reveal ⌥⌘R, rename F2, cut / copy / paste ⌘X / ⌘C / ⌘V, move to trash ⌘⌫ (Ctrl and Alt/Shift variants off macOS).

## Terminals

Terminal panes get every key that the app has no binding for. On Windows and Linux they also get Ctrl+T, Ctrl+W, Ctrl+B, Ctrl+K, Ctrl+O, Ctrl+Q, Ctrl+[ and Ctrl+], the control keys a shell or Vim relies on (Ctrl+W deletes a word, Ctrl+[ is Escape), and F6 / Shift+F6, which programs such as htop and mc use; outside a terminal those keys keep their command. On Windows, AltGr characters (`@`, `#`, `\`, `[`, `€`… on Spanish or German layouts) are typed into the terminal as text. From a terminal, use the Shift chords (Ctrl+Shift+T, Ctrl+Shift+W…) or the leader: ⌃Space `t` / `x` / `b` / `:` / `h`…`l`. The app takes ⌃Space for the leader, so a terminal program never sees it, not even a lone ⌃Space with no key after it: the guide waits for your choice instead of replaying it. Ctrl+Space (NUL) is what readline, emacs and tmux use for set-mark and as an alternative prefix, so if a program inside a terminal needs it, run tmux with another prefix in the app, or change the leader in `keymap.json` (see below) and keep ⌃Space for the program.

Leader `w` moves focus to the tab sidebar on the right, on the active tab. Up/Down or `j`/`k` highlight tabs across every workspace without switching; Enter opens the highlighted tab (switching workspace if needed), and Esc returns to the previous focus without changing anything. With tabs on top, the layout switches to the sidebar first. **Go to tab…** (palette) searches tabs by tab title or workspace name instead.

Leader `Shift+w` opens **Switch workspace…**, leader `a` opens **Create workspace**, and leader `Shift+d` opens **Delete workspace…**. Create workspace opens the folder explorer (the ⌘O one) at the focused pane's folder: move with the arrows (or `j`/`k`), `-` goes up, typing filters, and ⌘↵ (Ctrl+↵ off macOS, or **Open Gentle Shell here**) makes the shown folder a workspace, named after it and shown, or reuses it when it already exists. Esc closes the explorer and creates nothing. For new tabs, use leader `c` (Agent), `t` (Terminal), or `Shift+c` (Canvas).

The leader waits for its second key as long as you need: it is not on a timer.

## Coming from tmux / Neovim / Herdr TUI

- **Leader = tmux prefix.** ⌃Space plays the part of `C-b`: press it, release, press one key. `v` and `s` split like `%` and `"` (right and down); `x` closes like `&`/`x`; `n` / `p` / `1`–`9` switch windows (tabs) like tmux's `n`, `p` and digits; `h j k l` move between panes like `select-pane` with vim keys; `c` creates; `:` is the command prompt, here the palette.
- **Neovim window keys.** [Neovim's window help](https://github.com/neovim/neovim/blob/master/runtime/doc/windows.txt) uses `Ctrl+W` then `h j k l` to move left/down/up/right, `v` to split vertically (side by side), `s` to split horizontally (stacked), and `c` to close. Gentle uses the same movement and split letters after its leader, but closes with `x`.
- **Herdr TUI prefix.** [Herdr TUI's keybinding docs](https://herdr.dev/tui/docs/configuration/#keybindings) give `Ctrl+B` as the default prefix and show `Ctrl+Space` as a configuration example, not a default. Gentle defaults to `Ctrl+Space`. The docs' example uses `prefix+c` for a new tab and `prefix+n` for the next tab, as Gentle does, but uses `prefix+minus` for a horizontal split where Gentle uses leader `s`.
- **Pane mode** is Gentle's sticky move/resize layer: leader then Space, bare keys to work, Esc to leave.
- **Vim keys = netrw/oil habits.** `j k gg G h l` in lists once the setting is on; `s` and `u` stage and unstage as in fugitive and lazygit.
- **`:` opens the palette**, which also takes `/` agent commands, so `⌃Space :` is your command line.
- **Discoverability**: press ⌃Space and wait; the which-key panel shows the rest, like which-key.nvim.

## Custom key bindings

Bindings and the leader can be changed in `keymap.json` (palette: **Open keymap file**, then **Reload keymap**). The file documents itself: the app creates it with a commented reference of every action you can bind, its label, its default keys and its exact `action` name. Comments (`//`, `/* */`) and a comma before a closing bracket are allowed, so keep the notes you find useful. The format, action names, contexts and how to unbind a key are in [integrations: Keymap file](integrations.md#keymap-file-sharedkeymaprs-gentle_corekeymap). The cheat sheet and the palette hints show your bindings after a reload.
