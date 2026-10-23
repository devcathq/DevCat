# Files and previews

Browse the project's files, preview code, documents and images, edit text files and see who changed each line.

## The file tree

The **Files** section in the AI workspace sidebar shows the open folder. Folders load when you open them, and
files are marked with their Git status (modified, added, untracked).

- **Filter loaded files** narrows the tree by name.
- The eye button shows or hides files that Git ignores.
- Use the arrow keys to move, <kbd>Enter</kbd> to open a file or folder.

Right-click a file or folder for: **Open preview**, **Blame**, **File history**, **Copy path**, **Copy relative
path**, **Mention in chat** (copies `@path` for the agent's prompt) and **Reveal in Finder** (Explorer on Windows).

## Go to File

Press <kbd>⌘P</kbd> (<kbd>Ctrl+P</kbd>) to open any file by name, like VS Code's and Cursor's quick open. It searches
every file in the folder of the tab you're in (or the AI workspace's folder), skipping what Git ignores.

- Type a few letters of the name or path: `ffmt` finds `src/features/files/format.ts`. Matches in the file name
  come first.
- With an empty box, the files you opened lately are listed first.
- <kbd>↑</kbd> / <kbd>↓</kbd> to move, <kbd>Enter</kbd> to open, <kbd>Esc</kbd> to close.

## Previews

Click a file to open it in a preview tab. DevCat shows:

- **Code and text** in an editor with syntax highlighting. Edit and press <kbd>⌘S</kbd> (<kbd>Ctrl+S</kbd>) to
  save. If the file changed on disk while you were editing, DevCat offers **Overwrite** or **Reload (discard my
  edits)**.
- **Markdown** rendered, with a switch to the source.
- **Images** and **PDFs**.
- **Spreadsheets** (Excel, CSV) as a table.
- **Word documents** as formatted text.

The toolbar has **Copy path**, **Open with default app** and **Reveal in Finder**. For safety, *Open with
default app* refuses scripts and programs.

## Blame and history

- **Blame** shows who last changed each line and when, with an age heatmap. Click a line's commit to open it in
  the [Git](git.md) view.
- **File history** lists every commit that touched the file.
