//! The help texts, as data: one [`Page`] per app plus the general pages, the
//! system-wide key list, and the shell command table for `help <command>`.
//!
//! Rules for this file: describe only what the code does today, name the
//! limits, no promises. Every statement was checked against the app's source
//! when it was written; when an app changes, change its page with it.
//! Plain ASCII, so every line renders in the UI font and in the Terminal.

pub struct Section {
    pub head: &'static str,
    pub lines: &'static [&'static str],
}

pub struct Page {
    /// Stable id (used by `help::page_for`).
    pub id: &'static str,
    /// Short name in the navigation list.
    pub nav: &'static str,
    pub title: &'static str,
    pub summary: &'static str,
    pub sections: &'static [Section],
    /// (key, what it does) for this app; also collected on the shortcut page.
    pub keys: &'static [(&'static str, &'static str)],
    /// An app page (listed under "Apps") or a general page.
    pub app: bool,
}

/// Keys that work on the whole desktop.
pub const SYSTEM_KEYS: &[(&str, &str)] = &[
    ("F1", "Help for the window in front (again: close help)"),
    ("F2", "This shortcut overview"),
    ("Alt+Tab", "Switch window: hold Alt, tap Tab, release Alt"),
    ("Alt+1 .. Alt+4", "Switch to workspace 1 to 4"),
    ("Esc", "Close the launcher, a dialog, a name prompt, the tour or help"),
    ("Enter", "Launcher: open the first result"),
];

pub const SYSTEM_KEY_NOTES: &[&str] = &[
    "Alt+Tab and Alt+1..4 are not picked up while EuroText, EuroNotes, EuroInstall or a EuroFiles name prompt has the keyboard. Click another window first.",
    "The key names shown at the right of context-menu items (Ctrl C, Del, F5, ...) are labels. They are not connected to the keyboard yet: choose the menu item.",
    "There are no Ctrl shortcuts for copy, paste, save or undo yet.",
];

pub const POINTER_KEYS: &[(&str, &str)] = &[
    ("Click EU mark", "Open the launcher"),
    ("Click dock tile", "Open the app; click the tile of the active window to hide it"),
    ("Right-click", "Context menu for the desktop, a dock tile, a file, a text window"),
    ("Drag title bar", "Move the window; drop at the left or right edge for half the screen, at the top to maximize"),
    ("Drag corner", "Resize from the bottom-right corner"),
    ("Click status panel", "Open or close the notification list (clock, top right)"),
    ("Click ?", "Help for that window"),
];

pub const PAGES: &[Page] = &[
    Page {
        id: "overview",
        nav: "EuroOS at a glance",
        title: "EuroOS at a glance",
        summary: "A map of the desktop. F1 opens help for the window in front, F2 lists the keyboard shortcuts.",
        app: false,
        keys: &[],
        sections: &[
            Section {
                head: "Dock and launcher",
                lines: &[
                    "The dock on the left has eleven app tiles. Click a tile to open the app; click the tile of the window in front to hide that window again.",
                    "The EU mark at the top of the dock opens the launcher. Type to search apps, file names and text inside files. Enter opens the first result, a click opens any result, Esc closes.",
                    "Four apps have no dock tile and start from the launcher: EuroView, EuroPaint, Chromium browser and Install EuroOS.",
                ],
            },
            Section {
                head: "Windows",
                lines: &[
                    "Drag the title bar to move a window. Drop it at the left or right screen edge to fill that half of the work area, at the top edge to maximize.",
                    "Drag the bottom-right corner to resize. The three dots in the title bar close, minimize and maximize.",
                    "Closing or minimizing a built-in app hides its window; the app keeps its content. Open it again from the dock or the launcher. Closing an installed app or Chromium ends that program.",
                    "Alt+Tab switches between open windows: keep Alt down, tap Tab until the window is selected, release Alt.",
                ],
            },
            Section {
                head: "Workspaces",
                lines: &[
                    "Alt+1 to Alt+4 switch between four desktops. Each one remembers which windows were open on it; a workspace you visit for the first time is empty. The four dots at the bottom of the screen show the active one.",
                ],
            },
            Section {
                head: "Context menus",
                lines: &[
                    "Right-click the desktop: open the Terminal, take a screenshot (saved as /screenshots/shot-N.ppm, opens in EuroView), display settings, restore the last deleted file.",
                    "Right-click a file or folder in EuroFiles: open, copy the path, move to Trash. Right-click a dock tile: open.",
                    "Right-click in the Terminal or EuroText: Paste and Insert symbol.",
                ],
            },
            Section {
                head: "Notifications",
                lines: &[
                    "Messages appear at the top right for a few seconds. Click the status panel (the clock, top right) to open or close the list of earlier messages.",
                ],
            },
            Section {
                head: "Sign-in",
                lines: &[
                    "The lock screen checks the account password before the desktop starts. The preview image has a demo account 'euro' with password 'euro'; when no key is pressed it signs in by itself after a moment.",
                    "An installed system always waits for the password of the account created during installation.",
                ],
            },
            Section {
                head: "Apps and updates",
                lines: &[
                    "The app store is a signed package list on euro-os.eu. Store apps that are not installed show up in the launcher marked 'install'. In the Terminal: app store refresh, app store install <name>, app installed, app run <name>.",
                    "eupkg lists and installs packages from the package index stored on this system.",
                    "System updates need an installed system with its two system partitions: euroupdate status. On the preview image automatic updates are off.",
                ],
            },
            Section {
                head: "What the preview image keeps",
                lines: &[
                    "Started from the preview image alone, the file system lives in memory. Files, notes, settings, installed apps and the 'do not show again' choice of the tour are gone after a restart.",
                    "They are kept once EuroOS is installed on a disk (launcher: Install EuroOS). In a virtual machine with a virtio disk attached EuroOS uses that disk as its file system instead of memory; an empty disk is formatted for this.",
                    "Use the preview to try things out. Do not keep the only copy of important data on it.",
                ],
            },
            Section {
                head: "More help",
                lines: &[
                    "Terminal: help lists the commands by topic, help <command> explains one with an example, help tour shows the first-run tour again.",
                    "/home/euro/Welcome.txt is a short text version of this page.",
                ],
            },
        ],
    },
    Page {
        id: "shortcuts",
        nav: "Keyboard shortcuts",
        title: "Keyboard shortcuts",
        summary: "All keys that do something today, system-wide first, then per app. F2 opens this page from anywhere.",
        app: false,
        keys: &[],
        sections: &[],
    },
    Page {
        id: "security",
        nav: "Security badges",
        title: "Security badges and terms",
        summary: "What the labels on screen mean, and what they do not tell you.",
        app: false,
        keys: &[],
        sections: &[
            Section {
                head: "\"Protected\" in a title bar",
                lines: &[
                    "The green pill marks a window that the desktop lists as sandboxed. In this preview every window carries it. It is a fixed label: it does not change with what the app does and is not a scan result.",
                ],
            },
            Section {
                head: "Capabilities",
                lines: &[
                    "Programs that run outside the kernel (installed apps, Linux-compatible programs, agents) start with a fixed list of capabilities: console, file, net, procinfo. A program without 'net' cannot open network connections; one without 'file' cannot read or write files.",
                    "See the list per program with caps or apps in the Terminal, or in EuroBeheer under Apps.",
                    "The built-in desktop apps (EuroFiles, EuroText and the others in the dock) are part of the kernel. They are not confined by capabilities.",
                ],
            },
            Section {
                head: "Permission requests",
                lines: &[
                    "When a program asks for something it does not hold, a dialog appears with three choices: Allow once, This session, Deny. Granted permissions can be withdrawn in EuroBeheer under Apps (Revoke all permissions).",
                ],
            },
            Section {
                head: "EuroGuard",
                lines: &[
                    "EuroGuard is the network policy of the system: blocked domains, addresses and ports, plus rules per program. The system list is the file /etc/euroguard/system.conf.",
                    "Block a domain in EuroBeheer under EuroGuard, or with guard block <domain>. guard allow <domain> removes it. guard shows the policy and recent decisions.",
                ],
            },
            Section {
                head: "Status panel (top right)",
                lines: &[
                    "With a TPM in the computer the panel reads 'Your device is safe' (measured boot, encrypted, sandboxed). Without one it reads 'Capability-sandboxed' and states that full-disk encryption needs a TPM.",
                ],
            },
            Section {
                head: "\"protected\" in EuroFiles",
                lines: &[
                    "A file with this badge is immutable: it cannot be changed, renamed or deleted until the protection is removed. System files are protected at start-up. The Protect button sets or removes it for your own files under /home.",
                ],
            },
            Section {
                head: "Signed programs and packages",
                lines: &[
                    "Programs in /bin carry a signature that is checked before they run. Packages from the app store are checked against the developer key named in the signed store list, and their content hash is checked again at every start.",
                ],
            },
            Section {
                head: "Audit log",
                lines: &[
                    "Security events (sign-ins, capability checks, protection changes, agent tool calls) go into a log in which every entry includes the hash of the previous one. EuroLog shows it; audit prints it in the Terminal.",
                ],
            },
        ],
    },
    // ── Apps ────────────────────────────────────────────────────────────────
    Page {
        id: "files",
        nav: "EuroFiles",
        title: "EuroFiles",
        summary: "File manager for the EuroOS file system.",
        app: true,
        keys: &[("Enter", "Confirm a name or permission prompt"), ("Esc", "Cancel the prompt")],
        sections: &[
            Section {
                head: "Browse and open",
                lines: &[
                    "Places on the left jump to Home (/home/euro), System (/), Configuration (/etc) and Logs (/var). The path bar shows where you are.",
                    "Click a folder to open it. The first row of the list goes one level up.",
                    "Click a file to select and open it: text files open in EuroText, images in EuroView. Drag a file onto the EuroText window to open it there.",
                ],
            },
            Section {
                head: "Toolbar",
                lines: &[
                    "The toolbar shows as many buttons as fit. In a narrow window Perms, Protect and History are missing: widen or maximize the window.",
                    "New Folder, Rename and Perms ask for a name or an octal mode such as 644. Enter confirms, Esc cancels.",
                    "Copy or Cut, go to the target folder, then Paste. Copy works for single files, not for folders.",
                    "Delete removes the selected file at once, without the Trash. A folder is removed only when it is empty.",
                    "Protect makes the selected file immutable, or removes that again. It works for your own files under /home.",
                    "History lists the stored earlier versions of a file. Click a version to restore it; the content you had is kept as a new version.",
                ],
            },
            Section {
                head: "Right-click",
                lines: &[
                    "On a file: Open, Copy path, Move to Trash. Files in the Trash are kept in /var/trash.",
                    "On an empty part of the list: New folder, Restore last deleted (when the Trash is not empty), Refresh.",
                ],
            },
            Section {
                head: "Good to know",
                lines: &[
                    "The badge 'protected' marks an immutable file. An operation the system refuses (permissions, protection, quota) is reported as a notification.",
                    "Limits: one selection at a time, no keyboard navigation in the list, no search field (use the launcher to search).",
                ],
            },
        ],
    },
    Page {
        id: "notes",
        nav: "EuroNotes",
        title: "EuroNotes",
        summary: "Notes written as Markdown and shown formatted.",
        app: true,
        keys: &[
            ("Arrow keys", "Move the cursor"),
            ("Home / End", "Start or end of the line"),
            ("PgUp / PgDn", "Move several lines"),
            ("Del / Backspace", "Delete forward or backward"),
            ("Esc", "In the search field: clear the filter"),
        ],
        sections: &[
            Section {
                head: "Notes list",
                lines: &[
                    "Click a note on the left to show and edit it. '+ New note' below the list adds one. The x at the right end of a row deletes that note at once; the last remaining note cannot be deleted.",
                    "Search: click the field above the list and type to filter the list. Esc clears the filter, Enter returns to the note.",
                ],
            },
            Section {
                head: "Writing",
                lines: &[
                    "Type in the note on the right. Markdown is recognised: # and ## for headings, **bold**, *italic*, - for list items, > for quotes, `code`, and #tags.",
                    "Toolbar: B, I and <> put the markers around the word at the cursor. H1, H2, the bullet and the quote button change the current line. 'Aa' switches between a sans-serif and a monospace font.",
                ],
            },
            Section {
                head: "Where notes are stored",
                lines: &[
                    "/home/euro/notes/note-0.md, note-1.md and so on, one Markdown file per note. They are saved automatically after each change.",
                ],
            },
            Section {
                head: "Limits",
                lines: &[
                    "No undo. Paste and Insert symbol from the right-click menu insert into EuroText, not into the note.",
                ],
            },
        ],
    },
    Page {
        id: "clock",
        nav: "EuroClock",
        title: "EuroClock",
        summary: "Local time and date from the hardware clock, and a set of world clocks.",
        app: true,
        keys: &[],
        sections: &[
            Section {
                head: "What it shows",
                lines: &[
                    "The large clock is the local time; the cards below show the time in other zones with their UTC offset. Everything updates by itself.",
                ],
            },
            Section {
                head: "Limits",
                lines: &[
                    "Display only: nothing in this window can be clicked. Timer, stopwatch and alarm are not available in the window yet, and the list of zones cannot be changed.",
                ],
            },
        ],
    },
    Page {
        id: "web",
        nav: "EuroWeb",
        title: "EuroWeb",
        summary: "Built-in browser with its own HTML and CSS engine. It suits simple pages; for full websites use Chromium when its pack is attached.",
        app: true,
        keys: &[("Enter", "Load the typed address"), ("Esc", "Stop editing the address"), ("Backspace", "Delete the last character")],
        sections: &[
            Section {
                head: "Loading a page",
                lines: &[
                    "Type an address and press Enter. Typing starts a new address straight away; clicking the address bar first is not needed.",
                    "An address without http:// or https:// is loaded over plain HTTP. Type https:// in front for an encrypted connection. Redirects are followed.",
                ],
            },
            Section {
                head: "Tabs and forms",
                lines: &[
                    "Click a tab to switch to it; + opens a new tab. Tabs cannot be closed.",
                    "Click a text field on a page to type in it, and click the page's button to send the form.",
                ],
            },
            Section {
                head: "Limits",
                lines: &[
                    "Links cannot be clicked and pages do not scroll: only the top part of a long page is visible. There is no back button, no bookmarks and no downloads.",
                    "Scripts run once when the page loads, with a small JavaScript interpreter. Sites that depend on scripts or modern layout will not display correctly.",
                    "The desktop does not respond while a page is being fetched.",
                ],
            },
        ],
    },
    Page {
        id: "terminal",
        nav: "Terminal",
        title: "Terminal",
        summary: "The command line (EuroShell).",
        app: true,
        keys: &[("Enter", "Run the command"), ("Backspace", "Delete the last character")],
        sections: &[
            Section {
                head: "Getting started",
                lines: &[
                    "help lists the commands by topic. help <command> explains one command and gives an example, for instance help ls.",
                    "Commands run as the signed-in user. sudo <command> runs one command as root.",
                    "Pipes and redirection work: a | b, cmd > file, cmd >> file.",
                ],
            },
            Section {
                head: "Programs",
                lines: &[
                    "A name that is not a built-in command is looked up in /bin and run as a separate program with its own capabilities. caps lists the programs and what each may do.",
                    "Right-click in the window for Paste and Insert symbol.",
                    "keymap shows the keyboard layout; keymap be-azerty (or us, fr-azerty, de-qwertz) changes it.",
                ],
            },
            Section {
                head: "Limits",
                lines: &[
                    "No command history, no tab completion, no cursor movement within the line. Output that scrolls out of the window is gone; clear empties the window.",
                    "A command that waits for the network holds the whole desktop until it returns.",
                ],
            },
        ],
    },
    Page {
        id: "settings",
        nav: "EuroBeheer",
        title: "EuroBeheer (Settings)",
        summary: "Shows and changes the live state of the system: network policy, per-program access, network, system.",
        app: true,
        keys: &[("Enter", "EuroGuard section: block the typed domain"), ("Esc", "Stop editing the domain field")],
        sections: &[
            Section {
                head: "Sections",
                lines: &[
                    "EuroGuard: the current network policy. Type a domain and press Enter to add it to the block list.",
                    "Apps: click a program to see its capabilities and the permissions it was granted. 'Block all network' cuts that program off from the network; 'Revoke all permissions' makes it ask again next time.",
                    "Network: the state of the network connection, and a switch for the built-in HTTP server on port 80.",
                    "System: uptime, number of tasks, kernel memory and processor protection. Read-only.",
                ],
            },
            Section {
                head: "Good to know",
                lines: &[
                    "Changes apply at once; there is no Apply or Undo button. To unblock a domain use guard allow <domain> in the Terminal.",
                    "'Display settings' in the desktop menu opens the System section. Screen resolution and theme cannot be changed here.",
                ],
            },
        ],
    },
    Page {
        id: "calc",
        nav: "EuroReken",
        title: "EuroReken (Calculator)",
        summary: "Calculator. The result is recalculated while you type.",
        app: true,
        keys: &[
            ("0-9 . + - * / % ( )", "Enter the expression"),
            ("Enter or =", "Store the line and its result in History"),
            ("Backspace", "Delete the last character"),
            ("C or Esc", "Clear the expression"),
        ],
        sections: &[
            Section {
                head: "Use",
                lines: &[
                    "Click the buttons or type. Functions: sin, cos, tan, square root, ln, log, exp, absolute value, power (x^y), and the constants pi and e. A bracket that is still open is closed for the live result.",
                    "Memory: M+ adds the current result to the memory, M- subtracts it, MR puts the memory into the expression, MC clears it. The memory value is shown above the history.",
                    "History on the right keeps the last eight lines. Click a line to bring it back.",
                ],
            },
            Section {
                head: "Good to know",
                lines: &[
                    "The same engine is available in the Terminal: calc 2*(3+4).",
                    "Nothing is saved: the history is empty again after a restart.",
                ],
            },
        ],
    },
    Page {
        id: "agent",
        nav: "EuroAgent",
        title: "EuroAgent",
        summary: "Panel for handing a task to an agent, with every tool call checked against the agent's capabilities and written to the audit log.",
        app: true,
        keys: &[("Enter", "Run the typed task"), ("Esc", "Stop editing the field")],
        sections: &[
            Section {
                head: "Use",
                lines: &[
                    "Type a task in the field and press Enter. The panel lists which agent took it, each tool call with 'allowed' or 'denied', and the final answer.",
                ],
            },
            Section {
                head: "Limits",
                lines: &[
                    "This window runs a scripted demonstration model. No language model is contacted and the tool calls change nothing on the system.",
                    "The Allow and Deny buttons shown under a denied call cannot be clicked yet.",
                ],
            },
        ],
    },
    Page {
        id: "text",
        nav: "EuroText",
        title: "EuroText",
        summary: "Plain-text editor.",
        app: true,
        keys: &[
            ("Arrow keys", "Move the cursor"),
            ("Home / End", "Start or end of the line"),
            ("PgUp / PgDn", "Move several lines"),
            ("Del / Backspace", "Delete forward or backward"),
            ("Tab", "Insert four spaces"),
        ],
        sections: &[
            Section {
                head: "Open and save",
                lines: &[
                    "Open shows a file list: click a folder to enter it, click a file and then the Open button (or press Enter). Cancel, Esc or a click outside the list closes it. A text file clicked in EuroFiles, or dragged onto this window, opens here too.",
                    "Save writes to the file that is open. A text that was never opened from a file is saved as /home/euro/euro.txt. There is no 'Save as': to store a copy under another name, copy the file in EuroFiles.",
                    "The status bar shows the number of lines, the cursor position and 'unsaved changes'.",
                ],
            },
            Section {
                head: "Editing",
                lines: &[
                    "Click in the text to place the cursor. Right-click for Paste and Insert symbol.",
                ],
            },
            Section {
                head: "Limits",
                lines: &[
                    "No undo, no text selection, no search. Ctrl+S and other Ctrl shortcuts do nothing: use the buttons. Closing the window does not ask about unsaved changes; the text stays in the window until the system restarts.",
                ],
            },
        ],
    },
    Page {
        id: "monitor",
        nav: "EuroMonitor",
        title: "EuroMonitor",
        summary: "Read-only system status.",
        app: true,
        keys: &[],
        sections: &[
            Section {
                head: "What it shows",
                lines: &[
                    "Date and time, memory in use and free, the number of running tasks, the disk (or 'live mode' when EuroOS runs from memory), and the number of entries in the audit log.",
                ],
            },
            Section {
                head: "Limits",
                lines: &[
                    "There is nothing to click. For a list of processes use ps in the Terminal; kill <pid> stops a background program; free and df show memory and disk space.",
                ],
            },
        ],
    },
    Page {
        id: "log",
        nav: "EuroLog",
        title: "EuroLog",
        summary: "The most recent entries of the security audit log of this session.",
        app: true,
        keys: &[],
        sections: &[
            Section {
                head: "What it shows",
                lines: &[
                    "Sign-ins, capability checks, protection changes and agent tool calls, numbered in order. Each entry includes the hash of the one before it, so a later change to the log can be detected.",
                ],
            },
            Section {
                head: "Limits",
                lines: &[
                    "Read-only, and it shows only as many recent entries as fit in the window: make the window taller to see more. There is no scrolling, search or export here; audit in the Terminal prints the log.",
                ],
            },
        ],
    },
    Page {
        id: "view",
        nav: "EuroView",
        title: "EuroView",
        summary: "Image viewer for PNG, BMP, QOI, PPM/PGM and baseline JPEG files.",
        app: true,
        keys: &[],
        sections: &[
            Section {
                head: "Use",
                lines: &[
                    "Click an image file in EuroFiles to open it here. Opened from the launcher, the viewer shows the last image, at first /home/euro/pictures/euroos.png.",
                    "Toolbar: Fit scales the image to the window, 100% and 200% show it at that size, Rotate turns it a quarter turn per click, Prev and Next go through the other images in the same folder.",
                    "Screenshots from the desktop menu are PPM files in /screenshots and open here.",
                ],
            },
            Section {
                head: "Limits",
                lines: &[
                    "View only: rotation and zoom are not saved. Progressive JPEG files are not supported. No keyboard shortcuts; a large image at 200% cannot be moved around.",
                ],
            },
        ],
    },
    Page {
        id: "paint",
        nav: "EuroPaint",
        title: "EuroPaint",
        summary: "Drawing program with a fixed canvas of 640 x 470 pixels.",
        app: true,
        keys: &[],
        sections: &[
            Section {
                head: "Tools",
                lines: &[
                    "Brush and Pencil (1 pixel) draw freehand. Line, Rect, the filled rectangle and Ellipse: press, drag and release; the shape is previewed while you drag.",
                    "Bucket fills the area you click. Picker takes the colour under the pointer. Eraser paints white.",
                    "Brush size: the slider under the tools, 1 to 64 pixels.",
                ],
            },
            Section {
                head: "Colour",
                lines: &[
                    "The large field sets saturation (left to right) and brightness (top to bottom); the narrow strip next to it sets the hue. The row below shows recent colours; click one to use it again.",
                ],
            },
            Section {
                head: "Undo and save",
                lines: &[
                    "Undo and Redo step back and forward through up to 24 changes. Clear empties the canvas and can be undone.",
                    "Save always writes the PNG file /home/euro/pictures/painting.png and replaces the painting saved before. To keep a painting, rename or copy that file in EuroFiles before saving the next one.",
                ],
            },
            Section {
                head: "Limits",
                lines: &[
                    "No file chooser: the window cannot open an existing image or save under another name or format. The canvas size is fixed. No keyboard shortcuts.",
                ],
            },
        ],
    },
    Page {
        id: "install",
        nav: "Install EuroOS",
        title: "EuroInstall",
        summary: "Installs EuroOS from the running preview onto a disk of this computer. Start it from the launcher: Install EuroOS.",
        app: true,
        keys: &[
            ("Enter", "Next step"),
            ("Esc", "Previous step"),
            ("Arrow keys", "Choose in a list"),
            ("Tab", "Next field on the account page"),
            ("Space", "Encryption page: switch encryption on or off"),
        ],
        sections: &[
            Section {
                head: "Steps",
                lines: &[
                    "Welcome, Language, Keyboard, Disk, Account, Encryption, Summary, Install. Nothing is written to any disk before you confirm on the Summary page.",
                    "Disk: only a blank NVMe or SATA disk can be chosen. The chosen disk is erased completely.",
                    "Account: the first account is the administrator. The password needs 12 or more characters with upper and lower case, a digit and a symbol. It is typed in the keyboard layout chosen two steps earlier.",
                    "Encryption: with a TPM the disk key is sealed to this computer. Without a TPM the installation continues without encryption.",
                ],
            },
            Section {
                head: "After installing",
                lines: &[
                    "The installed system starts from the disk, keeps files across restarts, asks for the password at the lock screen and can receive system updates (euroupdate).",
                ],
            },
        ],
    },
    Page {
        id: "store",
        nav: "App store, packages",
        title: "App store, packages and updates",
        summary: "How software gets onto the system, and how it is checked.",
        app: true,
        keys: &[],
        sections: &[
            Section {
                head: "App store",
                lines: &[
                    "The store is a package list on euro-os.eu, signed with the EuroOS key. It needs a working network connection.",
                    "In the launcher, store apps that are not installed appear in the results marked 'install'. Choosing one downloads, checks and installs it; a notification reports the result. The desktop waits while this runs.",
                    "In the Terminal: app store refresh fetches the list, app store shows it, app store install <name> installs, app installed lists what is installed, app run <name> starts an app, app remove <name> removes one (administrator only).",
                ],
            },
            Section {
                head: "Checks",
                lines: &[
                    "A list with a wrong signature is refused. Each package must be signed by the developer key that the signed list names for it, and the hash of its program is checked at installation and again at every start.",
                    "A package states which capabilities it wants. See and limit them in EuroBeheer under Apps, or with app <name>.",
                ],
            },
            Section {
                head: "eupkg",
                lines: &[
                    "eupkg list shows the packages in the local package store; eupkg install <name> installs from the signed index stored on this system (/pkg/index.json). Without that index it reports that none is present.",
                ],
            },
            Section {
                head: "System updates",
                lines: &[
                    "euroupdate status shows the installed version and the update policy. Checking, staging and applying need the administrator (sudo) and an installed system; the preview image has no system partitions to update.",
                ],
            },
        ],
    },
    Page {
        id: "chromium",
        nav: "Chromium",
        title: "Chromium browser",
        summary: "A Chromium build that runs as a Linux-compatible program in its own window.",
        app: true,
        keys: &[],
        sections: &[
            Section {
                head: "Starting it",
                lines: &[
                    "Chromium is not part of the image. It needs the separate Chromium pack disk attached to the computer or virtual machine. Without the pack it does not start, and chrome in the Terminal reports that /pack/chrome is missing.",
                    "With the pack: choose 'Chromium browser' in the launcher, or type chrome in the Terminal. Starting takes a while.",
                ],
            },
            Section {
                head: "Good to know",
                lines: &[
                    "Mouse clicks and keys go to the browser while its window is in front. Closing the window ends the browser.",
                    "Chromium runs as a program outside the kernel with its own capability set, like any installed app.",
                ],
            },
        ],
    },
    Page {
        id: "suite",
        nav: "EuroSuite (preview)",
        title: "EuroSuite: Writer, Calc, Impress",
        summary: "Preview windows of the office apps.",
        app: true,
        keys: &[],
        sections: &[
            Section {
                head: "State of these windows",
                lines: &[
                    "Each window shows a built-in sample document with a toolbar. They are not interactive yet: the buttons do nothing, and nothing can be typed, opened or saved.",
                    "Opening a document file from EuroFiles brings the matching window to the front, but it keeps showing the sample, not your file.",
                ],
            },
        ],
    },
    Page {
        id: "hosted",
        nav: "Installed apps",
        title: "Installed apps",
        summary: "Windows of apps that were installed from a package, and of Linux-compatible programs.",
        app: true,
        keys: &[],
        sections: &[
            Section {
                head: "How they run",
                lines: &[
                    "These apps run as separate programs outside the kernel. EuroOS draws the window frame; the content, its buttons and its keys belong to the app, so this help cannot describe them.",
                    "Closing the window asks the app to stop; an app that does not react is ended by the system.",
                ],
            },
            Section {
                head: "Control",
                lines: &[
                    "Each app has a fixed list of capabilities. EuroBeheer under Apps shows them and can block the app's network access or revoke permissions it was granted. In the Terminal: apps and app <name>.",
                ],
            },
        ],
    },
];

// ── Shell command table (`help`, `help <command>`) ───────────────────────────

pub struct Cmd {
    pub name: &'static str,
    /// Other names for the same command, space-separated.
    pub alias: &'static str,
    /// Index into [`CMD_GROUPS`].
    pub group: u8,
    pub usage: &'static str,
    pub what: &'static str,
    pub example: &'static str,
}

pub const CMD_GROUPS: &[&str] = &["Files", "Text", "System", "Users", "Security", "Network", "Apps", "Programs"];

const fn c(name: &'static str, alias: &'static str, group: u8, usage: &'static str, what: &'static str, example: &'static str) -> Cmd {
    Cmd { name, alias, group, usage, what, example }
}

pub const CMDS: &[Cmd] = &[
    // Files
    c("ls", "", 0, "ls [path]", "list a directory", "ls /home/euro"),
    c("cat", "", 0, "cat <path>", "print a file", "cat /home/euro/Welcome.txt"),
    c("write", "", 0, "write <path> <text>", "create or replace a file with one line of text", "write /home/euro/todo.txt buy bread"),
    c("mkdir", "", 0, "mkdir <path>", "create a directory", "mkdir /home/euro/docs"),
    c("cp", "", 0, "cp <src> <dst>", "copy a file", "cp /home/euro/todo.txt /home/euro/todo.bak"),
    c("mv", "rename", 0, "mv <src> <dst>", "rename or move; replaces an existing file", "mv /home/euro/todo.txt /home/euro/list.txt"),
    c("rm", "", 0, "rm <path>", "delete a file (not via the Trash)", "rm /home/euro/todo.bak"),
    c("rmdir", "", 0, "rmdir <path>", "remove an empty directory", "rmdir /home/euro/docs"),
    c("touch", "", 0, "touch <path>", "create an empty file", "touch /home/euro/new.txt"),
    c("stat", "", 0, "stat <path>", "size, owner, mode and times of a file", "stat /home/euro/Welcome.txt"),
    c("find", "", 0, "find <dir> [-name <pattern>] [-type d|f] [-maxdepth N]", "search the file tree", "find /home -name *.txt"),
    c("df", "", 0, "df", "free space on the file system", "df"),
    c("chmod", "", 0, "chmod <octal-mode> <path>", "change permission bits (owner or root)", "chmod 600 /home/euro/todo.txt"),
    c("open", "", 0, "open <path>", "show the file type and the app that opens it", "open /home/euro/Welcome.txt"),
    c("trash", "", 0, "trash", "list files in the Trash (restore from the desktop menu)", "trash"),
    c("eurosnap", "", 0, "eurosnap [create [label] | rollback <id> | delete <id>]", "list, create, roll back or delete file-system snapshots", "eurosnap create before-cleanup"),
    c("versions", "", 0, "versions on|off|list|restore <path> [n]", "keep and restore earlier versions of a file", "versions list /home/euro/todo.txt"),
    c("fsck", "scrub", 0, "fsck [repair]", "check the file system (checksums and structure)", "fsck"),
    c("mount", "", 0, "mount [<devN> <mountpoint>]", "list or mount additional disks; umount <mountpoint> detaches", "mount"),
    // Text
    c("echo", "", 1, "echo <text>", "print text", "echo hello > /home/euro/hello.txt"),
    c("grep", "", 1, "grep <pattern> [file]", "print the lines that contain the pattern", "cat /home/euro/Welcome.txt | grep help"),
    c("head", "", 1, "head [-n N] [file]", "first lines of the input", "cat /home/euro/Welcome.txt | head -n 5"),
    c("tail", "", 1, "tail [-n N] [file]", "last lines of the input", "seq 10 | tail -n 3"),
    c("wc", "", 1, "wc [-l] [file]", "count lines, words and bytes", "cat /home/euro/Welcome.txt | wc -l"),
    c("sort", "", 1, "sort [file]", "sort lines", "ls /bin | sort"),
    c("uniq", "", 1, "uniq [file]", "drop repeated adjacent lines", "ls /bin | sort | uniq"),
    c("cut", "", 1, "cut -d<char> -f<n> [file]", "select a field of each line", "cat /etc/passwd | cut -d: -f1"),
    c("tr", "", 1, "tr <from> <to>", "replace characters", "echo hello | tr l L"),
    c("seq", "", 1, "seq <n>", "print the numbers 1 to n", "seq 5"),
    c("tee", "", 1, "tee <file>", "copy the input to a file and pass it on", "seq 3 | tee /home/euro/n.txt | wc -l"),
    c("sha256sum", "", 1, "sha256sum [file]", "SHA-256 checksum of the input", "echo euroos | sha256sum"),
    c("base64", "", 1, "base64 [file]", "encode the input as base64", "echo hello | base64"),
    // System
    c("uname", "", 2, "uname [-a|-r|-m|-n]", "system name and version", "uname -a"),
    c("hostname", "", 2, "hostname", "name of this computer", "hostname"),
    c("date", "", 2, "date", "date and time from the hardware clock", "date"),
    c("uptime", "", 2, "uptime", "time since start", "uptime"),
    c("free", "mem", 2, "free", "memory in use and free", "free"),
    c("ps", "", 2, "ps", "running processes and tasks", "ps"),
    c("kill", "", 2, "kill <pid>", "stop a background program", "kill 12"),
    c("dmesg", "", 2, "dmesg [N]", "last N lines of the kernel log", "dmesg 20"),
    c("journal", "journalctl", 2, "journal", "the structured system journal", "journal"),
    c("lspci", "", 2, "lspci", "PCI devices", "lspci"),
    c("lsdev", "eurodevice", 2, "lsdev", "devices and the drivers bound to them", "lsdev"),
    c("lsblk", "blkid", 2, "lsblk", "disks and partitions", "lsblk"),
    c("hwprobe", "", 2, "hwprobe", "hardware and driver inventory", "hwprobe"),
    c("services", "euroctl", 2, "services", "state of the system services", "services"),
    c("battery", "power", 2, "battery", "battery and mains adapter state", "battery"),
    c("keymap", "", 2, "keymap [us|be-azerty|fr-azerty|de-qwertz]", "show or set the keyboard layout", "keymap be-azerty"),
    c("locale", "", 2, "locale", "language and regional formats", "locale"),
    c("notify", "notifications", 2, "notify", "earlier notifications", "notify"),
    c("clip", "", 2, "clip [text]", "show the clipboard history, or copy text to it", "clip hello"),
    c("clear", "", 2, "clear", "empty the Terminal window", "clear"),
    c("reboot", "", 2, "reboot", "restart the computer", "reboot"),
    c("shutdown", "poweroff", 2, "shutdown", "switch the computer off", "shutdown"),
    // Users
    c("whoami", "", 3, "whoami", "name of the signed-in user", "whoami"),
    c("id", "", 3, "id", "user id and group id of the session", "id"),
    c("login", "su", 3, "login <user> <password>", "sign in as another user in this Terminal", "login euro euro"),
    c("logout", "", 3, "logout", "end that session and return to the desktop user", "logout"),
    c("sudo", "", 3, "sudo <command>", "run one command as root (desktop user and root only)", "sudo euroupdate status"),
    c("sessions", "", 3, "sessions", "list the sessions", "sessions"),
    c("eurousers", "", 3, "eurousers [list | add <user> | passwd <user> | lock <user> | unlock <user> | del <user>]", "manage accounts", "eurousers list"),
    c("chown", "", 3, "chown <uid> <path>", "change the owner of a file (administrator)", "sudo chown 1000 /home/euro/todo.txt"),
    c("quota", "", 3, "quota [set <uid> <blocks>]", "show or set the disk quota per user", "quota"),
    // Security
    c("caps", "euroguard", 4, "caps", "programs with their capabilities, and the network policy", "caps"),
    c("apps", "", 4, "apps", "all programs with capabilities and network state", "apps"),
    c("app", "", 4, "app <name> [revoke]  |  app installed | run <name> | store ...", "inspect or limit one program; manage installed apps", "app store"),
    c("guard", "", 4, "guard [block <domain> | allow <domain>]", "show the network policy; block or unblock a domain", "guard block ads.example.com"),
    c("firewall", "eurofw", 4, "firewall", "packet filter rules and counters", "firewall"),
    c("audit", "", 4, "audit", "print the security audit log", "audit"),
    c("vault", "", 4, "vault [get <label>]", "the secrets store; reading needs the capability", "vault"),
    c("europol", "", 4, "europol [explain <CAP>]", "the policy that maps roles to capabilities", "europol"),
    c("immutable", "euroimmutable", 4, "immutable", "the protected (immutable) system files", "immutable"),
    c("integrity", "", 4, "integrity", "check the system files against their recorded hashes", "integrity"),
    c("eurohealth", "", 4, "eurohealth", "health summary of the system", "eurohealth"),
    c("portal", "", 4, "portal", "permission requests and grants", "portal"),
    // Network
    c("net", "", 5, "net", "network interface, address and state", "net"),
    c("netstat", "", 5, "netstat", "DNS cache and network counters", "netstat"),
    c("ping", "", 5, "ping <host>", "send an echo request (IPv4); ping6 for IPv6", "ping euro-os.eu"),
    c("nslookup", "resolve", 5, "nslookup <host>", "look up the address of a name", "nslookup euro-os.eu"),
    c("fetch", "wget", 5, "fetch <host> [save-path]", "HTTP GET; with a path, save the answer to a file", "fetch example.com"),
    c("https", "", 5, "https <host>", "HTTPS GET over TLS 1.3", "https euro-os.eu"),
    c("httpd", "", 5, "httpd", "switch the built-in HTTP server on port 80 on or off", "httpd"),
    c("vpn", "eurovpn", 5, "vpn", "state of the VPN tunnel", "vpn"),
    c("wifi", "eurowifi", 5, "wifi", "wireless state", "wifi"),
    // Apps
    c("eupkg", "", 6, "eupkg [list | install <name>]", "packages from the signed index on this system", "eupkg list"),
    c("euroupdate", "eup", 6, "euroupdate [status | check | policy <auto|ask|manual> | rollback]", "system updates; all but status need sudo", "euroupdate status"),
    c("install", "", 6, "install <package>", "Terminal only: install a bundled signed program into /bin", "install msum"),
    c("euroinstall", "", 6, "euroinstall", "show the installation plan (the installer window does the installing)", "euroinstall"),
    // Programs
    c("calc", "", 7, "calc <expression>", "Terminal only: calculate", "calc 2*(3+4)"),
    c("js", "", 7, "js <code>", "Terminal only: run JavaScript in the built-in interpreter", "js 1+2"),
    c("chrome", "chromium", 7, "chrome [url]", "Terminal only: start Chromium (needs the Chromium pack disk)", "chrome"),
    c("doom", "", 7, "doom", "Terminal only: start the DOOM port full screen, if installed", "doom"),
    c("print", "", 7, "print [text]", "print on a network printer (IPP Everywhere)", "print hello"),
    c("scan", "", 7, "scan [path]", "scan from a network scanner (eSCL) into a file", "scan /home/euro/scan.png"),
    c("agent", "euroagent", 7, "agent", "state of the agents and their tool calls", "agent"),
    c("llm", "", 7, "llm", "state of the local language-model connection", "llm"),
    c("wasm", "", 7, "wasm <file.wasm>", "run a WebAssembly program in the sandbox", "wasm /agents/demo.wasm"),
    c("help", "", 7, "help [command | tour]", "this list, one command, or the desktop tour", "help ls"),
];
