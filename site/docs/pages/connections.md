# Database connections

Connect to PostgreSQL, MySQL / MariaDB, SQLite, SQL Server, MongoDB and Redis, through SSH or a VPN if needed.

## Add a connection

1. Click **Connections** in the left bar, then **New connection** (or the **+** at the top of the list).
2. Pick the database type.
3. Fill in host, port, user, password and database. For SQLite, pick the file.
4. Click **Test connection** to check it, then save.

The connection appears in the list. Click it to connect and browse its databases, schemas and tables.

### Options in the connection form

- **Project**: which project the connection belongs to.
- **Color**: a color tag, so production stands out from staging.
- **SSL mode** (**TLS** for MongoDB and Redis): encrypt the connection to the server. *Allow self-signed
  certificates* is there for test servers.
- **Connect through an SSH tunnel**: reach a database that only listens inside a private network (SSH host, user,
  password or private key).
- **Network**: a VPN, SSH tunnel or Kubernetes port-forward to connect first. DevCat starts it for you before
  connecting. See [VPN and tunnels](vpn-tunnels.md).
- **Read-only connection**: refuse anything that would change data.

> **Note:** Passwords are stored in the system keychain (macOS Keychain, Windows Credential Manager), never in plain
> files.

## The connection tree

Expand a connection to see databases, schemas, tables, views, functions, procedures, triggers, sequences and
indexes (collections for MongoDB, keys for Redis).

Right-click any item for its actions. Common ones:

| On | Actions |
| --- | --- |
| A connection | **Connect** / **Disconnect**, **New query**, **Refresh**, **Data transfer…**, **Schema compare…**, **Edit connection…**, **Duplicate**, **Color**, **Move to project** |
| A schema (or a database without schemas) | **New query**, **New table…**, **ER diagram**, **Backup / restore, import / export…**, **Schema compare…** |
| A table | **Open data**, **Design table**, **View DDL**, **Export…**, **Truncate…** |

Press <kbd>⌘R</kbd> (<kbd>Ctrl+R</kbd>) to refresh the selected item, or the whole tree when nothing is
selected. **Refresh all** at the top of the list does the same for every connection.

## Bookmarks

Big servers have many databases. Right-click a database, schema, table, view, collection, Redis key or routine →
**Add bookmark** to pin it. The star at the top of the list (**Show only bookmarked**) then shows just your
bookmarks, so you can stay connected to everything and still find the important ones fast. **Remove bookmark**
unpins one.

## History and saved queries

Under the connection list, open **Queries** to see every query you ran, per connection, and the queries you
saved with <kbd>⌘S</kbd>. Click one to open it again. Drag the line above **Queries** to give it more room.
