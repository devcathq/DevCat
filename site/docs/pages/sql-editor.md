# SQL editor

Write and run SQL with autocomplete, see several result sets at once, explain and format queries.

![The SQL editor with a query and its results](../img/query.png)

## Run a query

1. Right-click a connection, database or schema → **New query**.
2. Check the **Connection** and **Schema** menus in the toolbar.
3. Type your SQL. Autocomplete knows the tables and columns of the connected schema.
4. Press <kbd>⌘↵</kbd> (<kbd>Ctrl+Enter</kbd>) to run the statement under the cursor, or the selected text.

Press <kbd>⇧⌘↵</kbd> (<kbd>Ctrl+Shift+Enter</kbd>) to run the whole script. Each statement that returns rows gets
its own result tab. **Stop** cancels a running query.

## More tools

| Tool | How |
| --- | --- |
| Explain the statement | <kbd>⌘E</kbd> (<kbd>Ctrl+E</kbd>) |
| Format SQL | <kbd>⇧⌥F</kbd> (<kbd>Shift+Alt+F</kbd>) or the toolbar button |
| Save the query | <kbd>⌘S</kbd> (<kbd>Ctrl+S</kbd>); it then appears under *Queries* in the Connections sidebar |
| Limit rows | the row limit in the toolbar (*Maximum rows fetched per result set*) |

## Results

Results open in the [data grid](data-grid.md): sort, filter, copy as CSV / TSV / JSON / INSERT statements, or
export them. The messages tab shows row counts and errors.

> **Tip:** Every query you run is kept under *Queries* in the Connections sidebar, so you can find yesterday's query again.
