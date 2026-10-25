<div align="center">

<img src="packaging/thundergoose.svg" width="104" alt="Thundergoose logo">

# Thundergoose

### Email as conversations with people.

Your inbox, sent mail and folders, regrouped into chats: one per person or group, both directions,
across all your accounts.

![Rust](https://img.shields.io/badge/Rust-2021-b7410e?logo=rust&logoColor=white)
![Slint](https://img.shields.io/badge/UI-Slint-2379f4)
![Platforms](https://img.shields.io/badge/Linux%20%C2%B7%20Windows%20%C2%B7%20macOS-native-6a4cf5)
![IMAP/SMTP](https://img.shields.io/badge/IMAP%20%2B%20SMTP-OAuth2%20%C2%B7%20password-0d9488)

<img src="docs/chat-dark.png" alt="A conversation with Maya: a photo inline, replies as bubbles, group mail marked in the thread" width="100%">

<sub>All names and addresses in the screenshots are made up (<code>thundergoose --demo</code>).</sub>

</div>

---

Most mail clients show folders full of separate messages. Thundergoose shows **people**. Every
message belongs to a *peer*: the set of everyone on it except you. Pick a peer and you see your
whole history with them, as a chat:

| Peer | What you see |
| --- | --- |
| **Alice** | Every mail between you and Alice, whichever of your addresses it used. |
| **Alice, Bob** | The group conversation with exactly those two. It also shows up in Alice's and Bob's own chats, in bubbles with an accent border and a *Group · …* link. |
| **Yourself** | Notes and files you send between your own addresses. |

Thundergoose does one thing: email. No calendar, no feeds, no folders to manage.

## Highlights

<table>
<tr>
<td width="50%"><img src="docs/group-light.png" alt="Group conversation in the light theme"></td>
<td width="50%"><img src="docs/yourself-dark.png" alt="The Yourself conversation with From and To pickers"></td>
</tr>
<tr>
<td><b>Groups are chats too.</b> A thread with three people is its own conversation, and each
message also appears in every participant's personal chat.</td>
<td><b>Yourself.</b> A built-in chat for notes to self. Pick which of your addresses it comes
from and which one gets it; the bubbles show the way it went.</td>
</tr>
</table>

- **Favorites, Muted, Ignored, one click away.** Hover a chat in the list for ☆ / 🔔 / ⊘ quick
  buttons, or use the chat header or the right-click menu. Favorites are pinned on top; the rest
  is sorted by newest message. *Muted* chats move to their own tab and still count unread mail
  there, but never notify or play a sound. *Ignored* chats are silent and left out of every count.
- **Website icons.** Contacts at companies and organisations get their site's icon, fetched once
  from their domain and cached. Addresses at Gmail, Outlook, GMX, Proton and about a hundred other
  mailbox providers keep their initials. Can be switched off in Settings.
- **Pictures inline.** Image attachments up to 10 MiB show right in the bubble (rotated the way
  the camera meant); click to open, hover to save. Everything else is a one-click attachment chip.
- **Your Thunderbird accounts, without setting anything up.** Servers, passwords and OAuth
  sign-ins are taken from the Thunderbird profile ([details below](#your-thunderbird-accounts-without-setting-anything-up)).
- **Or add accounts in the app.** *Settings → Add account* finds the servers for an address:
  presets for Gmail/Outlook, the provider's autoconfig file, Thunderbird's ISP database, the
  domain's MX record (so Google Workspace and Microsoft 365 domains are recognized), and as a
  last resort probing the usual host names. Sign in with Google/Microsoft in the browser or with
  a password; both servers are tested before the account is added.
- **All accounts in one list.** A peer who wrote to two of your addresses is one conversation.
  Replies go out from the address the mail was sent to. Filter by account with the chips under
  the search box.
- **Chat-style bubbles.** Quoted history ("On … wrote:", "Am … schrieb", Outlook headers, `>`
  blocks) is cut from each reply, so a bubble shows only what is new. *Show full message* brings
  the quote back. *Open original* shows the HTML version in your browser with remote content
  blocked.
- **Reply like a chat.** Type and press <kbd>Ctrl</kbd>+<kbd>Enter</kbd>. Subject and threading
  headers are filled in; change the subject to start a new topic.
- **Search** across names, addresses, subjects and message text. Type an address you have no
  conversation with yet to start one.
- **Notifications** with a short chime, both optional.
- **Read state syncs both ways.** Opening a conversation marks it read on the server.
- **Newest first, cached.** Sync walks back in time: new mail first, then older headers in
  chunks, with the newest bodies downloaded in between, so recent mail is readable within
  seconds. A restart shows your mail immediately and only fetches what changed.

<p align="center"><img src="docs/settings-light.png" alt="Settings: accounts, notifications, appearance" width="80%"></p>

## Try it

```sh
nix develop
cargo run --release -- --demo     # made-up mail, no servers contacted
cargo run --release               # your accounts
```

Without Nix you need a Rust toolchain and the usual X11/Wayland/fontconfig development
libraries. `nix build` produces a wrapped binary with a desktop file and icon.

`thundergoose --screenshots docs` renders the pictures in this README from the demo data,
without a display.

## Your Thunderbird accounts, without setting anything up

On first start Thundergoose reads your Thunderbird profile and takes over:

| What | How |
| --- | --- |
| IMAP/SMTP servers, ports, TLS/STARTTLS, user names, identities, display names | `prefs.js` |
| Saved passwords | `logins.json`, decrypted with the profile's `key4.db` |
| OAuth2 sign-ins (Gmail, Google Workspace, Microsoft 365 / Outlook) | Thunderbird's refresh tokens, used with Thunderbird's own OAuth client registration, so they stay valid |
| Contact names | Thunderbird address books and collected addresses |

There's nothing to log in to. If a token expires, Thundergoose first picks up the newer one that
Thunderbird may have stored. Otherwise *Settings → Sign in* runs the same browser sign-in
Thunderbird uses (localhost redirect). Password accounts can get a new password there too.
*Settings → Re-import from Thunderbird* reads the profile again.

Profiles are found at `~/.thunderbird`, the Flatpak and Snap locations,
`~/Library/Thunderbird` (macOS) and `%APPDATA%\Thunderbird` (Windows). If you set a primary
password in Thunderbird, enter it next to *Import from Thunderbird* in Settings.

Run `thundergoose --check` to test the IMAP and SMTP login of every account without changing
anything, and `thundergoose --detect you@example.com` to see which servers would be used for an
address.

## Where data lives

| | |
| --- | --- |
| Linux | `~/.local/share/thundergoose/` |
| macOS | `~/Library/Application Support/Thundergoose/` |
| Windows | `%APPDATA%\Thundergoose\data\` |

`mail.sqlite` is the cache, `accounts.json` the account list, `settings.json` the appearance,
`favicons/` the website icons.
`secrets.json` (mode 0600) holds the passwords and refresh tokens, protected the same way
Thunderbird's are when no primary password is set. Set `THUNDERGOOSE_HOME` to use another
directory.

## Keyboard

| Keys | Action |
| --- | --- |
| <kbd>Ctrl</kbd>+<kbd>K</kbd> / <kbd>Ctrl</kbd>+<kbd>F</kbd> | Search |
| <kbd>Enter</kbd> in search | Open the first result |
| <kbd>Alt</kbd>+<kbd>↑</kbd> / <kbd>Alt</kbd>+<kbd>↓</kbd> | Previous / next conversation |
| <kbd>Ctrl</kbd>+<kbd>N</kbd> | New conversation |
| <kbd>Ctrl</kbd>+<kbd>Enter</kbd> | Send |
| <kbd>F5</kbd> / <kbd>Ctrl</kbd>+<kbd>R</kbd> | Check for new mail |
| <kbd>Esc</kbd> | Close dialog / cancel new conversation |

## Layout of the code

| File | |
| --- | --- |
| `src/tb.rs`, `src/nss.rs` | Thunderbird profile import; NSS `key4.db` / `logins.json` decryption |
| `src/oauth.rs`, `src/creds.rs` | OAuth2 refresh and browser sign-in; credentials for the network threads |
| `src/imapc.rs`, `src/sync.rs` | IMAP connections; one sync thread per account |
| `src/store.rs` | SQLite cache: mails (one per Message-ID), server locations, names |
| `src/parse.rs` | Envelopes, bodies, quote stripping, attachments |
| `src/send.rs` | SMTP sending (password or XOAUTH2) |
| `src/autoconfig.rs` | Finding servers for a new address |
| `src/notify.rs` | Desktop notifications and the chime |
| `src/favicon.rs` | Website icons and the list of mailbox providers |
| `src/demo.rs`, `src/shots.rs` | Demo data; headless screenshots |
| `src/app.rs` | UI glue |
| `ui/*.slint` | The interface (theme shared with [Neobrush](https://github.com/WhiteBlackGoose/Neobrush)) |

Fonts: Inter (OFL). Icons: Lucide (ISC).
