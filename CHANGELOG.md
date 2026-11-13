# Changelog

All notable changes to sandb. Versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

- Fixed: in a chat on a MongoDB database, the assistant's list-tables and describe-table steps now answer. Before, they failed with "Not a SQL connection", so the assistant could not see the collection or field list.
- Fixed: sandb no longer stops at launch when the connections file has a connection that this version cannot read. A newer version can write such a connection, for example a MongoDB connection that version 0.11.0 does not know. sandb now starts with the other connections and hides that connection. When sandb saves the list, it keeps the hidden connection unchanged. If you install the newer version again, the connection is still there.
- Fixed: in the Data view of a MongoDB collection, a filter on a UUID field now finds the documents. Before, the filter sent the value as a string, so it found no document.
- Fixed: when you stop an approved AI write on MySQL, or when a statement in it fails, the message no longer says "Nothing changed" if an earlier statement in the batch can already be committed. MySQL commits some statements (for example `CREATE TABLE`) at once. The message now tells you to check the database.

## [0.12.0] - 2026-11-11

- New: you can now sort the connection list. Drag a connection up or down in the sidebar. You can also select a connection and press Alt+↑ or Alt+↓. sandb saves the order. If the save fails, the list shows the old order again and a message shows the error.
- Changed: the MongoDB **JSON** view is easier to read. Values have the same colors as the SQL editor: strings, numbers, `true`/`false`, `null` and type names such as `ObjectId` and `BinData` each have a color. A line at the left of each open level shows the nesting. A collapsed document shows its `_id`, and a collapsed document or array shows its size, for example `7 fields` or `2 items`. A line separates the documents, and the row under the pointer is highlighted.
- Fixed: in the MongoDB grid, you can now open and edit the cells of a document that has a `binData` `_id`. Before, a double-click on a nested value, for example `[2 items]`, showed "This document's _id type (binData) cannot be edited in the grid". sandb now finds the document with its full `BinData(…)` value.
- New: you can now see the nested values of a MongoDB document in the **Columns** view. Double-click a document or array cell, for example `[2 items]`, or right-click it and choose **View value**. A window shows the value as a tree that you open one level at a time. You do not have to turn on **Edit**, so this also works on a read-only connection. In edit mode, a double-click still opens the editor.
- Fixed: while the assistant writes a reply, you can now scroll up in the chat. Before, each new part of the reply moved the chat back to the bottom. The chat follows the reply only when you are at the bottom. Scroll back to the bottom, send a message or open a chat to follow it again.
- Fixed: in the SQL editor, when you scroll a long line to the side, the code no longer shows through the line numbers.
- New: **Format query** formats the text in the editor. Press ⇧⌘F, or right-click the editor and choose **Format query**. If you select text, only the selection changes. Undo (⌘Z) restores the old text.
- SQL gets one clause on each line, with the dialect of the connection: PostgreSQL, MySQL or SQLite. Mongosh text puts a document or an array on more lines when it does not fit in 80 columns. Statements stay separate, with a blank line between them.
- The format changes only spaces and line breaks. It keeps the case of keywords, comments, strings and numbers, for example `1.0`. In mongosh, it removes a comma after the last item of a document or an array. If sandb cannot read the text, for example a bracket that is not closed, the text does not change and a message shows the error.
- You can now approve or deny a write that the assistant proposes. Before, the card of a proposed write showed no buttons, and the write did not run. Now the card has **Deny** and **Approve and run**. The reply waits for your decision.
- **Approve and run** runs the SQL in one transaction and commits it at once, also on a production connection. The write does not go into the open transaction of a tab. If one statement fails, no statement of the write stays. The step then shows the number of changed rows, and the assistant gets the result. An approved MongoDB write is one statement instead: it runs alone, so a statement that changes many documents is not one transaction.
- **Deny** runs nothing. If you do not decide in 10 minutes, the write counts as denied. The Stop button also cancels a write that waits, and stops a running write before it commits.
- Fixed: a tool step that fails with an error, for example a query error or a database that refused the connection, no longer shows **Denied**. It shows **Failed** in red, with the error message under the step. **Denied** now means only that you denied a proposed write.
- Before an approved write runs, sandb checks the connection again. A connection that became read-only refuses the write. SQL that contains `BEGIN`, `COMMIT`, `ROLLBACK` or `SAVEPOINT` is refused, because the write runs in its own transaction.
- The approval card no longer shows a fixed "≈ 214 rows" estimate. That number was sample data.
- New: MongoDB connections, first step. You can save, test, edit, duplicate and delete a MongoDB connection. Enter the host, port, user, password and auth source, or paste a `mongodb://` or `mongodb+srv://` connection string. **Test connection** shows the server version and the server type: standalone, replica set or sharded.
- A password in a connection string goes into the credential store. The connections file keeps the string without the password.
- The assistant can now use MongoDB databases, alone or in one chat with SQL databases. Add a MongoDB database in the chat database list, or open the chat on a tab that is on MongoDB. On MongoDB, the assistant reads with one mongosh statement, for example `db.orders.aggregate([...])`, through the same parser and read-only check as the editor. It can draw a chart from the result and propose a write. A proposed MongoDB write runs only after you approve it. To combine SQL and MongoDB data, the assistant queries each database and combines the results in its answer.
- `db.runCommand({ explain: ... })` counts as a read only when the command inside is `find`, `aggregate`, `count` or `distinct`, and it has no `$out` or `$merge`. Other `explain` commands, for example of `mapReduce`, are refused on a read-only connection and in the assistant, because they can run JavaScript or write.
- The **AI write access** switch shows in the MongoDB connection form and in Settings, as for SQL connections.
- You can now browse a MongoDB connection in the tree. It shows the databases that your user can read, then a **Collections** folder and a **Views** folder. Time series collections show as collections.
- Expand a collection to see its fields. MongoDB has no fixed columns, so sandb reads a random sample of up to 1,000 documents or 8 MB and lists each field path, for example `address.city` or `items[].sku`. Each field shows its types, for example `decimal128 | int32 | string`, and the percent of sampled documents that have it. A line above the fields tells you the sample size. A `$jsonSchema` validator adds the fields and types that it names.
- You can now query MongoDB in the editor. Pick a MongoDB connection as the tab target and write mongosh-style statements, for example `db.orders.find({ status: 'paid' }).sort({ _id: -1 }).limit(20)`. sandb reads the text with its own parser and never runs JavaScript. It supports `find` with `sort`, `limit`, `skip`, `project`, `hint`, `collation` and `count`, `findOne`, `aggregate`, `countDocuments`, `estimatedDocumentCount`, `distinct`, `getIndexes`, `stats`, the insert, update, replace and delete methods, `findOneAndUpdate`, `findOneAndReplace`, `findOneAndDelete`, `bulkWrite`, the index methods, `drop`, `renameCollection`, `db.createCollection`, `db.dropDatabase`, `db.runCommand`, `db.adminCommand`, `show dbs`, `show collections` and `use`. Values can use `ObjectId()`, `ISODate()`, `new Date()`, `NumberInt()`, `NumberLong()`, `NumberDecimal()`, `UUID()`, `BinData()`, `Timestamp()`, `MinKey()`, `MaxKey()` and `/regex/`. A syntax error tells you its line and column.
- MongoDB results have two views. **Columns** shows each field as a column, with nested fields to three levels (`address.city`) and arrays as a short text (`[3 items]`). Point to a cell to see its type. **JSON** shows each document as a tree that you open one level at a time. Right-click a cell and choose **Copy value** to copy its full value.
- A MongoDB result stops at the row limit, or at 16 MB of documents, whichever comes first. **Load more**, sorting by a column header and **Cancel** work as for SQL. A page that stops at 16 MB has no **Load more**, because a larger row limit brings no new documents; use a projection or a filter. Cancel also stops the operation on the server. If the server does not answer, Cancel still stops the run at once, and sandb tries to stop the server operation for 2 seconds. After `use <database>` in a run, **Load more**, sorting and edits use that database, not the database of the tab.
- Double-click a collection in the tree to open its documents in a data tab. The filter bar works on fields, for example `address.city` = `Jakarta`, with a comparison, `contains`, `startsWith`, `endsWith`, null and not-null. **Refresh** reloads, and **Count rows** is absent, because sandb would have to read every document to count them.
- **Export CSV** of a MongoDB result writes every document with the same columns as the grid. **Export…** next to it saves the documents as CSV, a JSON array or NDJSON (one document on each line), in relaxed or canonical Extended JSON. Canonical keeps every type, for example int64 and decimal.
- A MongoDB collection now has a **Properties** view, as a SQL table has. It shows **Fields**, **Indexes**, **Validator**, **Options**, **Stats** and **Script**. A view shows **Fields**, **Options** and **Script**.
- On a connection that can write, you can add and drop an index of a collection, and add, edit or remove its validator. The dialog shows the mongosh command before it runs, and a production connection asks you to confirm. The `_id_` index cannot be dropped. The dialog runs the command only if it is one statement.
- **Script** shows the mongosh text that makes the collection again: `db.createCollection(...)` with its options and validator, then one `createIndex` for each index. **Generate script** in the tree opens this text in an editor.
- In the connection tree, a MongoDB collection has **Export collection…**, **Export script…** and **Export script with data…**. Export script with data adds `insertMany` statements with at most 1,000 documents each, and a view exports only its script. The export reads the collection with a cursor and does not keep it in memory.
- The editor now reads `Double(x)`, `Code("…")` and `RegExp("pattern", "flags")`. `createIndex` accepts `weights`, `default_language` and the other options of text, wildcard and geo indexes.
- The filter bar and the grid edits of MongoDB now always use `db.getCollection("name")`, so a collection with a name such as `version` or `stats` works.
- On a read-only MongoDB connection, sandb runs only statements that read. It refuses a write, an aggregate with `$out` or `$merge` at any depth, a command that is not on its list of read commands, and `$where`, `$function` or `$accumulator`, which run JavaScript on the server. MongoDB has no read-only session, so this check of the app is the only barrier inside it; use a database user with the `read` role for full protection. The MongoDB connection form shows this note.
- On a production MongoDB connection, sandb asks you to confirm before a statement that changes data runs. The dialog lists each statement and warns about an `updateMany` or `deleteMany` with an empty filter, `drop`, `dropIndexes` and `renameCollection`.
- You can now edit MongoDB documents in the grid, as you edit SQL rows. Click **Edit** below the result of one `find()` on a collection. The result of an aggregate, a view or a projection that computes fields cannot be edited.
- Double-click a cell to change its value. A list next to the value shows the BSON type of the cell, for example `int64`, `decimal128`, `date` or `objectId`. The value keeps its type when you save. To change the type, select a different type in the list. A cell whose type the editor cannot write (`binData`, `regex`) can change only with **Edit as JSON…**.
- A cell that holds a document or an array opens an editor with mongosh text, for example `{ qty: NumberLong("3"), at: ISODate("2026-11-10T00:00:00Z") }`. Right-click any cell and choose **Edit as JSON…** to open the same editor. **Document** adds a new document with this editor. **Delete** removes the selected documents.
- **Save** shows the MongoDB commands before they run. On a replica set or a sharded cluster, all changes run in one transaction. If one change fails, no change stays. A standalone server has no transactions, so the dialog tells you that the changes run one by one. If one change fails, the changes before it stay saved, and the message tells you how many.
- Each update checks the old value of each changed field. If another user changed the document after you loaded it, sandb does not save the change and shows a conflict.
- If you cancel a run while a write statement waits for the server, sandb waits up to 2 seconds for its reply. If no reply comes, the message says that the statement may have changed data and that you must reload to check.
- MongoDB Atlas works: tested on an Atlas cluster with `mongodb+srv://` and TLS. **Test connection** answers in about 1 second and shows the server version. Browse, Properties, queries, writes, indexes, grid edits in a transaction, and **Cancel** work as on a local server.
- While a MongoDB save runs, the **Save changes** dialog shows "Saving changes…" and a **Cancel** button, and it cannot close. Cancel stops the save, and the transaction rolls back, so no change stays. If the cancel comes while the server commits, the message says that the save may have applied and that you must reload to check. A SQL save has the same busy state and Cancel as a MongoDB save; a cancel before COMMIT rolls the batch back. In a tab with an open transaction, Cancel undoes only the save, and the other changes of the transaction stay.
- Unsaved grid edits now stay with their tab. **New query**, opening a table in a new tab, and a switch to another tab no longer ask you to discard the edits of the current tab, and the edits are there when you come back. sandb asks before you lose edits of any tab: when you close the tab, quit the app, or change the host or database of its connection. If a new result replaces the result of a tab, its edits are dropped and the grid tells you.
- Fixed: the results pane moved 53 pixels to the left after you closed the **Save changes** dialog while the AI panel was open.
- You can now drop a database in the editor with `db.dropDatabase()`, with an optional `{ writeConcern: … }` document. sandb asks you to confirm on a production connection, refuses it on a read-only connection, and the assistant runs it only after you approve it.
- A MongoDB read that the assistant runs with `db.runCommand` or `db.adminCommand` now has a server time limit, so the server stops it at the same time as the chat, also for a command that ignores the cancel comment.
- A production MongoDB connection no longer asks you to confirm a read command, and a read-only connection and the assistant now accept these: `listDatabases`, `hello`, `isMaster`, `connectionStatus`, `hostInfo`, `getParameter`, `currentOp`, `getCmdLineOpts`, `whatsmyuri`, `connPoolStats`, `top` and `listCommands`. Before, sandb treated them as unknown commands.
- The MongoDB write confirmation now shows the database of each statement. Before, `db.dropDatabase()` and `drop()` could hide a `use` of another database, so you confirmed a drop without seeing which database it would remove.
- The connection tree now updates after a run that changes structure: a create, drop, rename or index change in MongoDB, `CREATE`, `DROP`, `ALTER`, `RENAME` or `TRUNCATE` in SQL, a grid save, an approved assistant write, or a MongoDB insert that makes a new collection or database. Open nodes stay open. A **Properties** view also reloads after such a change, also when it was closed at the time of the change.

## [0.11.0] - 2026-11-09

- The editor gutter now has a fold arrow at the first line of each statement that uses more than one line, as in VS Code. It replaces the Run button. Click the arrow to fold the statement to its first line. Click it again to unfold the statement. To run a statement, use ⌘↵.
- New fold shortcuts: ⌥⌘[ folds or unfolds the statement at the cursor. ⌃⌥[ folds all statements. ⌃⌥] unfolds all statements.
- Fixed: the assistant could show a chat on a different database than the active tab, for example after you start the app, open a tab on a different database, or open an old chat from history. Now, when the active tab moves to a database that the open chat does not use, the assistant starts a new chat on the database of the tab. **Explain with AI** does the same check when you click it: if the chat has messages on a different database, it starts a new chat; if the chat is empty, it changes the chat database to the database of the tab. The old chat stays in history.
- The row numbers in the results grid now stay at the left edge when you scroll to the right. They keep the color of the row: selected, new, deleted or failed.

## [0.10.0] - 2026-11-09

- Production work is easier to see. When a tab uses a production connection, the app shows these signs:
  - A red line under the tab in the title bar.
  - A red line at the top of the main panel, for table data, table properties and the query editor.
  - A **production** tag in the table header, before the connection name.
  - A red line along the tree under a production connection, from its databases down to its tables.
- Fixed: in the editor right-click menu, **Run statement at cursor** used two lines. The menu is now wider.
- New: **Export SQL** in the results footer, next to **Export CSV**. It writes all rows of the current SELECT as `INSERT` statements, with no row limit. Use the file to copy rows to a different environment, for example from dev to staging. The export needs the desktop app and a SELECT. Before you choose the file, a dialog lets you set these options:
  - The target schema and table. The query table is the default. Clear the schema to omit it.
  - The columns to export.
  - What to do when the row is already in the target: fail, skip the row, or update it (upsert on the key columns you choose; the primary key is the default).
  - The number of rows in each `INSERT` statement.
  - A transaction (`BEGIN` … `COMMIT`) around the file, and the header comment.

## [0.9.0] - 2026-11-09

- Fixed: a Run from an assistant SQL block could run the query on the database of the active tab, not on the database of the chat. If the reply has no step with a database, the block now uses the chat database when the chat has only one. If the active tab is on a different database, **Run in active tab** opens a new tab on the database of the block. The menu tells you this.
- Fixed: **Open** and **Open and run** on a saved query did not open a new tab when the query was already open in a tab. They selected that tab again. Now they always open a new tab. They use the active tab only if it is empty and on the same database.
- Fixed: **Run in active tab** and **Insert into editor** on an assistant SQL block added the query below the current text without a `;`. When you ran all of the editor, the two queries ran as one statement and failed with `syntax error at or near "SELECT"`. Now the app adds `;` after the last statement first.
- The assistant now shows a **Query used** block under each reply that ran a query when steps are hidden. You can copy the SQL or send it to the editor without a second question.
- The steps header of a reply now shows the total reply time. The time that the queries took shows as **query … ms**.
- While the model writes a tool call, the assistant shows what it does (for example **Writing query…** or **Reading schema…**), not only **Thinking…**. The backend sends a new `tool_starting` chat event for this.
- The assistant does not suggest write prompts (for example "Delete cancelled orders…") when it is in read-only mode.
- SQL code blocks in the assistant panel now wrap long lines. The copy and Run buttons do not cover the text.
- The result pane does not show an empty **Results** tab when there are no results. Use the **Results** button at the bottom to show or hide the pane.
- The **Read** and **Write** labels in the editor toolbar now have a tooltip.
- Table tabs do not use a query number. A new query tab gets the lowest free number (**Query 1**, **Query 2**, …).
- The connection tooltip in the tree now shows the environment. Production connections show a **production** tag.

## [0.8.0] - 2026-11-07

- SQL code blocks in the assistant panel now have a Run button. Read blocks open a small menu with **Run in active tab** (adds the query below the current text, with one blank line) and **Run in a new tab** (on the database of the reply). Picking an item runs the query at once.
- Write blocks are inserted into the editor instead of being executed. The menu says so and offers **Insert into editor**, so you can review the SQL and run it yourself. Non-SQL blocks keep Copy only.
- The menu shows whether the block reads or writes and how many statements it holds. Each Run choice shows the database and environment it will use.
- A run with several statements now gets one **Result** tab per statement that returns rows, plus a **Messages** tab for writes. Each result tab can be closed on its own; the last one hides the results pane.
- The editor has a small Run button in the gutter at the first line of each statement, like DBeaver's execute-statement action. It runs only that statement and leaves the editor text unchanged.
- ⇧⌘↵ runs the query like ⌘↵, but the results open in new result tabs. The result tabs of earlier runs stay open.
- The red notice above the results footer, for example "Only a simple single-table SELECT with plain columns or * is editable", now closes by itself after 4 seconds. A notice about a failed save on a row stays until you close it.
- After an update, the first start shows a "What's new" dialog with the changes since the last version you opened. "Got it" or Esc closes it, and it does not open again for that version.
- The release notes on GitHub now show the changes of that version from this changelog. Before, they only said "See CHANGELOG.md".

## [0.7.3] - 2026-11-05

- Fixed: the SQL editor did not always show the cursor in the macOS app. Now the editor draws the cursor itself, so you can always see it.

## [0.7.2] - 2026-11-05

- In a narrow assistant panel, "Read-only" and "Writes need approval" stay on one line. The model name gets shorter with "…" and stays before the "i" button. The send button keeps its size.
- Fewer Keychain prompts after an update. sandb now keeps all saved passwords and the AI API key in one encrypted file, `secrets.enc`, in the data folder. The key of that file is the only sandb entry in the credential store of the operating system. Before, each password had its own entry, and macOS asked for each entry again after each update. Now macOS asks one time per update.
- On the first start of this version, sandb moves the old Keychain entries into `secrets.enc` and then removes them. macOS can ask one more time for each old entry. If the move stops, the old entries stay, and sandb tries again at the next start.
- Fixed: the notes of 0.5.0 said that the fixed code signature keeps the "Always Allow" permission after an update. This is not true. The certificate is self-signed, so macOS keeps the permission for one build only.

## [0.7.1] - 2026-11-05

- The editor completes column names in the statement, not only functions and keywords. In the `SELECT` list, `WHERE`, `GROUP BY`, `ORDER BY`, a join condition or `SET`, it lists the columns of the tables in that statement.
- When the statement has an alias, the completion shows the columns of the table behind it. If two tables have a column with the same name, the editor inserts the name with its table or alias, for example `o.id`, so the query stays valid.
- With no table in the statement, Ctrl+Space lists the columns of the whole database. Typing alone does not show that list.
- After `FROM`, `JOIN`, `UPDATE` and `INSERT INTO`, the editor still completes table names. After `table.` or `alias.` it still completes the columns of that one table. A table written as `public.orders` uses the columns of the snapshot's `orders`. If the snapshot has no columns for a table, the editor asks the database once and keeps the answer.

## [0.7.0] - 2026-11-05

- The model menu of the assistant moved from the header to the bottom of the message box, next to "Read-only". The menu opens upward.
- The keyboard shortcuts of the assistant are in a list now. Click the "i" button next to the send button to show the list.
- "Put the SQL in the editor" in the assistant now adds the SQL at the end of the editor. It does not replace your query. A blank line separates the two.
- A code block in an assistant reply has a copy button. Move the pointer over the block to show it.
- SQL code blocks in an assistant reply have colors, as in the editor. The block language (`sql`, `postgresql`, `mysql`, `sqlite`) sets the SQL dialect. A block without a language gets colors when it starts with a SQL statement, for example `SELECT` or `WITH`. Blocks in other languages show without colors.
- The assistant can show only the results of a reply. Click the list button in the assistant header, or press ⇧⌘H. Each reply then shows one line with the number of steps, followed by the answer, charts and errors. The steps and the text before the last data step do not show. Click the steps line to open the steps of that reply. A write that waits for your approval and a question that waits for your answer always show. sandb keeps this setting after a restart.
- The SQL in assistant steps has the same colors as the editor. A long statement scrolls in its box.
- Each finished step shows a green check mark. Before, finished "Describe table" and "List tables" steps showed a gray database icon, so they looked unfinished.
- A line now separates the assistant header from the messages.
- A click on a column header of an `EXPLAIN` or `SHOW` result does not show the error "Sorting requires a SELECT query" now. sandb can sort only a `SELECT` result, so these headers do not sort.
- To sort a result, click the sort icon in the column header. A click on the column name does not sort now.
- You can select columns and rows in a result, as in DBeaver, and copy them with ⌘C. Click a column name to select the column. Click a row number to select the row. Hold ⌘ to add or remove one, or hold ⇧ to select a range. When you select rows and columns, ⌘C copies only the cells in the selected rows and columns. sandb copies the values without the header, with tabs between the columns. A click on a cell clears the selection.

## [0.6.0] - 2026-11-05

- The macOS app also builds for Intel Macs. A release now ships an Apple Silicon build and an Intel build (the Intel disk image is named `sandb_<version>_x64.dmg`). Both update through the same `latest.json`.
- You can choose the AI model from a list. sandb gets the list from your AI server (`GET /models`). In Settings > AI, click the Model field. In the assistant, click the model name at the top. Type to filter the list. Use the arrow keys and Enter, or click a model.
- If the server does not list your model, type its full name and select "Use …". If sandb cannot get the list, the menu shows the error, and you can still type a name. The refresh button gets the list again.
- The assistant and Settings use the same model. A new model applies from the next question.

## [0.5.0] - 2026-11-05

- Releases now have a fixed code signature. After you update, macOS does not ask for your password again for each saved connection password. You must click "Always Allow" one more time after you install the first version with this change.
- Settings has tabs now: AI, Safeguards, Appearance and Updates. The "Update" button in the title bar opens the Updates tab. The model name in the assistant opens the AI tab.
- Appearance: you can show an image behind the app. Click Settings > Appearance > Background > Image, then choose a file. sandb makes the image smaller and keeps it on this computer.
- Appearance: the desktop app can make its window see-through. Click Background > See-through. Optionally, macOS blurs the desktop behind the window.
- With a background, sliders set the transparency. "Tint" sets the canvas color over the background. "Panel opacity" sets the opacity of the editor, results and assistant panels. Below 100%, the panels become frosted glass. With an image, "Panel blur" sets the blur of the glass and "Image blur" sets the blur of the image, both from 0% to 100%.
- The theme now stays after a restart.
- Fixed: when you asked for a chart, the assistant sometimes did not make one. It wrote "the chart is above", but the chat showed no chart. Now the prompt tells the assistant to call `render_chart` each time that you ask for a chart, graph or plot. It must not say that a chart or query result exists if its tool call did not succeed.

## [0.4.2] - 2026-11-05

- Fixed: autocomplete after `table.` or `alias.` showed no columns for some tables. On a large database, the schema snapshot keeps only the first 2000 columns, so later tables had no columns. Now the editor gets the columns of such a table from the database when you need them. This also applies to a PostgreSQL table outside `public` when you write it without its schema and its name is unique.

## [0.4.1] - 2026-11-05

- The Rust crate version now agrees with the app version. Before, the crate showed 0.1.0.

## [0.4.0] - 2026-11-05

- The results grid has no "Load more" button now. When you scroll near the last row, the grid gets the next rows. The footer shows the row count and the time of the last run, as before. While rows load, the footer shows "Loading more…". With pending edits, the grid does not load more rows.
- A double-click on a connection in the connection tree does not open "Edit connection" now. To edit a connection, use the "…" menu or right-click the connection.
- The connection tree puts tables, views and PostgreSQL materialized views in separate folders. Each folder shows its count. The Tables folder starts open. The other folders start closed. Before, all objects were in one long list, and materialized views showed as views.
- Each folder shows 100 objects. Click "Load 100 more" at the end of the folder to show the next 100. The row also shows how many objects are visible, for example "100 of 1235".
- PostgreSQL: the connection tree hides partitions. The parent table stays in the Tables folder. At the end of the folder, click "Show N partitions" to show them. To hide them again, put the pointer on the Tables folder and click the partition button. The choice applies to all connections and stays after a restart. The tooltip of a partition shows its parent table.
- Quick search (⌘K) now finds tables in all databases that are open in the connection tree. Before, it found only the tables of the active tab's database. Each table shows its connection and database. Press Enter to open the table in its own connection. The active database still gives all its tables, also the tables that you did not open in the tree.
- Fixed: "Drop materialized view…" now runs `DROP MATERIALIZED VIEW`. Before, it ran `DROP VIEW`, and PostgreSQL refused it. The assistant also sees materialized views as "(materialized view)" in its schema summary.
- The assistant can use more than one database in one chat. The databases can be on different connections. Above the chat box, each database of the chat shows as a chip. Click "+ Database" to add or remove databases. You can choose up to 8.
- A new chat starts with the database of the active tab. While the chat is empty, this database changes when you change the tab. When you add or remove a database, the chat keeps your choice. You can change the databases at any time. The change applies to the next question.
- With more than one database, the assistant names the database in each tool step. Each query runs on one database. The assistant cannot join tables of two databases in one SQL statement. It queries each database and then compares the results in its answer.
- Each step shows its database. "Put in editor", "Open in the results grid" and "Run again" use the database of that step, not the database of the active tab.
- Write mode stays a setting of each connection. With more than one connection in a chat, "Read-only" asks which connection can get writes.
- Chat history now shows all chats, not only the chats of the active connection. Each chat shows its databases. Chats from older versions keep their connection.
- PostgreSQL: the results grid, CSV export and the assistant show PostGIS `geometry` and `geography` values as WKT, as `ST_AsText` does, for example `MULTIPOLYGON(((130.8 -3.4,…)))`. Before, they showed as hex EWKB. The grid formats only the first 4 KB of a large geometry, so large polygons do not make the grid slow. SQL dumps keep the EWKB.
- When you edit a geometry cell, sandb keeps the SRID of the column. It uses the SRID of a typed column, for example `geometry(MultiPolygon,4326)`. For a plain `geometry` column, it uses the SRID of the old value. If you type EWKT with `SRID=…;`, sandb uses that SRID.
- A text cell that the grid shows cut off (it ends with "…") can now be edited. When you start the edit, sandb loads the full value of that one cell by its primary key. Before, the grid showed "Truncated text cells are read-only". If you save the full value without a change, the row does not change.
- The app has a new icon: an amber spark on a dark square. The title bar shows the same spark next to "sandb". Before, it showed a database icon.
- Fixed: when the assistant panel is wide, the Table Properties tables now scroll to the right. Before, the last columns were cut off and you could not see them. The assistant panel also becomes narrower when the window has no space for it. Before, the right part of the panel went out of the window.
- Fixed: text fields no longer change what you type. Before, macOS made "paid" into "Paid" in the filter value. Now the filter values, cell editors, search boxes, names, connection fields, Settings fields and the chat box have no automatic capitals, no autocorrection and no spell check.
- The assistant can make charts. It runs one read-only query and shows the result in the chat as a bar, line, area, pie or scatter chart. A chart can show more than one series and stacked series. Charts use the colors of the app theme and keep up to 1,000 rows. They stay in the chat history.
- Charts also work with the "schema" privacy setting. The chart shows the real values to you, but the model gets only the column names and the row count.
- Ask the assistant for a report, and it writes one document with a title, a summary, sections, charts and tables of key figures.
- Each assistant reply has an "Export PDF" button with a text label. The PDF has the reply text and its charts in the order of the chat, on A4 pages with page numbers. Charts stay sharp at any zoom. The desktop app asks where to save the file. In the browser, the file downloads. When you ask for a PDF, the assistant tells you to use this button.
- Fixed: while the assistant writes its text, the chat shows "Writing…". Before, the chat showed no sign of work after a result table.
- Fixed: Markdown tables in assistant replies now fill the width of the chat panel. A wide table scrolls.
- The assistant now knows the date of your computer. The prompt gives today's date, the weekday and the time zone. When you ask about "last month" or "this year", the assistant starts from that date, writes the exact date range in the SQL and tells you the range.
- The PDF uses the built-in PDF fonts. They show Latin text only. Other characters, for example Chinese or Japanese, show as "?".
- In a table Data view, each foreign key cell shows an arrow icon. Click the icon to open the referenced table with a filter for that row. If a tab already shows that table, that tab gets the filter and loads again. NULL values and unsaved changes do not show the icon.
- Table Properties has an "ER Diagram" section, as in DBeaver. It shows the table in the middle, the tables that refer to it on the left and the tables that it refers to on the right. Each box lists the columns with their types and marks primary key and foreign key columns. Lines connect the foreign key columns to the columns that they refer to. Put the pointer on a line to see the foreign key name and its columns. Drag a box header to move the box. "Reset layout" puts all boxes back. Double-click a related table to open it. Use the zoom buttons, ⌘/Ctrl + scroll or a trackpad pinch to zoom from 25% to 200%. Click the percentage to go back to 100%. "Fit to view" makes the full diagram visible.
- Saved queries. Press ⌘S (Ctrl+S) in the editor to save the query with a name. The query keeps its connection and database. After the first save, ⌘S saves your changes at once. ⇧⌘S saves a copy as a new query.
- The sidebar has a "Connections | Queries" switch. The Queries list groups saved queries by connection and shows the name, the start of the SQL and the time of the last save. Type in the filter box to find a query by name or SQL.
- Click a saved query to open it. If a tab already shows the query, that tab comes to the front. Right-click a query, or use its "…" button, to open and run it, rename it (F2), duplicate it, copy its SQL or delete it. Undo in the notice brings back a deleted query.
- A tab with unsaved changes to a saved query shows a dot in place of its X. When you close that tab, sandb asks first. The toolbar shows "Saved" when the tab has no changes. Rename a saved tab with a double-click, and the saved query gets the new name too.
- The command palette (⌘K) lists the saved queries and has a "Save query" action. Saved queries stay after the app restarts.
- Drag a results-grid column border to resize it. For keyboard resizing, focus the border and press the Left or Right arrow key.
- Fixed: opening a cell editor no longer changes the row height or column widths.
- Fixed: on a live connection, the first double-click edit no longer resets the column widths that you set.
- Fixed: an assistant read now shows all the rows that the backend sends (up to 50), not only the first 10. The table scrolls and its header stays visible.
- The assistant now looks at the schema first. Before it queries a table, it calls `describe_table` for that table, and it calls `list_tables` when it does not know the table.
- Assistant tool steps show as a numbered timeline with a status and a duration for each step. Steps stay closed. Click a step to open it. Only steps that wait for your approval and failed steps open automatically.
- Each step has "Call details": the tool name, the call id, the JSON arguments, the result, the duration and the error code.
- Read steps have actions: copy the SQL, put it in the editor, open it in the results grid in a new tab, run it again, and copy the rows as CSV.
- Assistant result tables align numbers to the right and show NULL as the grid does. Click a cell to see its full value. A note tells you when more rows exist.
- Assistant composer: the text box grows as you type. Up in an empty box brings back your last question. Each reply has Copy and Ask again. The suggestions use the tables of the current connection.
- Double-click a cell in a table Data view or a simple query result to edit it. Save applies all row changes in one transaction. Live connections use primary keys and bound values.
- Click a column header to sort the full result before the row limit. Refresh and Load more keep the sort.
- Table Data views now include a filter bar. Add filters by column, operator and value, then apply them together. Refresh, Load more and CSV export keep the active filters.
- Assistant replies now render Markdown tables, lists, headings, links and code blocks. Unsafe HTML is removed. The AI prompt also tells the model not to repeat rows that the query result table already shows.
- Security: the desktop app now keeps connection passwords and the AI API key in the credential store of the operating system. This is the Keychain on macOS, the Credential Manager on Windows and the Secret Service (GNOME Keyring or KWallet) on Linux. Before, the app kept them in `credentials.enc`, and the key for that file was in the app. On the first start, the app moves the passwords from `credentials.enc` to the credential store and then removes the file. If the move fails, the file stays and the app tries again at the next start. On Linux, a Secret Service must run. macOS can ask you once to let sandb use the Keychain.
- Table Properties can now change the table structure. Each section has an add button: Add column, Add constraint (unique, primary key or check), Add foreign key and Add index. The dialog shows the SQL before it runs. Hover a row and click the trash button to drop that column, constraint, foreign key or index. The change commits at once. On a production connection, you must first select a checkbox. Read-only connections and views do not show these buttons. SQLite cannot add or drop constraints and foreign keys on an existing table, so these two sections show a note.
- Table Properties can now edit a column, constraint, foreign key or index. Hover a row and click the pencil button. The dialog opens with the current values, and it shows the SQL before it runs. If nothing changed, Save stays off. A name-only change uses a rename. Other changes use `ALTER COLUMN` (PG), `CHANGE COLUMN` (MySQL) or a drop and add in one statement. MySQL cannot drop and add a foreign key with the same name in one statement, so the change uses two statements and a temporary name. On PG and SQLite, a change with more than one statement runs in one transaction. Limits: SQLite can only rename a column and can only edit a plain index. MySQL always names the primary key PRIMARY. An index with expressions, a sort order, WHERE, INCLUDE or FULLTEXT can only get a new name. MySQL `CHANGE COLUMN` removes AUTO_INCREMENT, COMMENT and ON UPDATE, and the dialog shows a warning about this. PG exclusion constraints have no edit button.
- The AI provider setting now has two options: "OpenAI-compatible" and "Anthropic-compatible". The Ollama, LM Studio, OpenRouter, OpenAI and Custom presets are gone. These servers all use the OpenAI format, so use "OpenAI-compatible" with their base URL. Anthropic-compatible uses `POST /messages` with the `x-api-key` and `anthropic-version` headers. Test uses `GET /models`. When you change the option, a base URL and model that you typed stay. Saved settings with an old preset load as OpenAI-compatible, with the same URL, model and key.

## [0.3.0] - 2026-11-04

- A new sidebar button in the title bar, next to the app name, hides or shows the sidebar. The command palette (⌘K) also has a "Toggle sidebar" action. The connection tree keeps its expanded nodes when you hide the sidebar.
- The results area has a tab strip above the grid, as in DBeaver. The tab shows the name of the source table, or "Results". The X on the tab hides the results. This tab replaces the X button at the top-right corner of the grid.
- The line between the editor and the results has two arrow buttons. The up arrow makes the results area as large as possible, and the down arrow makes the editor as large as possible. Click the other arrow to go back to the previous heights.
- Fixed: a query with its own `LIMIT` larger than 200 showed only 200 rows. The run now fetches the number of rows in the `LIMIT`, up to 100,000. Queries without a `LIMIT` still show 200 rows first, and "Load more" fetches more.
- Tables in the connection tree now open in a table tab, as in DBeaver. A single click only selects the table. Double-click, ⌘-click (Ctrl-click on Windows and Linux), Enter or "View data" opens it. If a tab for the table is already open, that tab comes to the front.
- A table tab has two views. "Data" shows the rows of the table and has a Refresh button. "Properties" shows the Columns, Constraints, Foreign Keys, References (foreign keys of other tables that point to this table), Indexes and DDL of the table.
- Fixed: a right-click on an empty part of the app showed the WebView menu (Reload, Play All Animations). Now this menu shows only in text fields and on selected text, for Cut, Copy and Paste.
- Fixed: ⌘W (Ctrl+W on Windows) closed the app window. Now it closes the active tab, as in a browser. The menu item is File > Close Tab. To close the window on macOS, use Window > Close Window (⇧⌘W).
- You can drag the left edge of the assistant panel to change its width (280 to 640 px). The arrow keys also move the edge when it has focus.
- The assistant keeps a history of chats for each connection. The clock button shows the list. Click a chat to open it again, or delete it. The pen button starts a new chat. The history stays after the app restarts.
- The assistant asks for write approval inside the chat, not in a dialog. The card shows the SQL, the connection, the statement type and the approximate number of rows. For a statement that removes data, the card shows a red warning, and Deny has the focus.
- The assistant can ask you a question in the chat. For example, if a table name does not exist, it shows the available tables as buttons. You can click one, type your own answer, or skip the question.
- The assistant now uses your AI server. Before, the chat gave scripted replies and did not send a request to the model. The assistant can list tables, describe a table, and run read-only queries through the AI read guard. It can propose a write, but it does not run it. Approval of AI writes comes in a later phase. The Stop button stops a reply.
- Fixed: the Test button in Settings showed "Connected" for any model name, also a wrong one. Now Test asks the server for its model list (`GET /models`). If the server does not list the model, Test shows "Failed" and the models that the server has.
- Settings saves the AI provider, base URL, model, privacy mode and limits. The desktop app keeps the API key in the encrypted `credentials.enc` file. The dev bridge keeps it in memory.
- With "Schema only", the model gets the column names and the row count of a query, but no row values. Query errors do not go to the model with their database text.

## [0.2.0] - 2026-11-03

- You can drag the edge of the sidebar to change its width.
- You can drag the line between the editor and the results to change their heights.
- A new footer below the editor has a Results button. It shows or hides the results grid. When you run a query, the grid opens.
- A close button (X) at the top-right corner of the results grid hides the grid.
- Right-click in the editor to open a menu with these items:
  - "Run statement at cursor" runs the statement under the cursor. If you selected text, the item is "Run selection".
  - "Run all statements" runs all the statements in the editor.
  - "Run SQL script from file…" runs a `.sql` file. The toolbar shows a progress bar with the number of completed statements.
  - Cut, Copy, Paste and Select all.
- Right-click a table or view in the connection tree to open a table menu with these items:
  - View data, Count rows and Copy name.
  - "Generate DDL", "Generate SELECT" and "Generate INSERT" open the SQL in a new query tab. They do not run it.
  - "Export DDL…" saves the CREATE statements to a `.sql` file. "Export as SQL (DDL + data)…" also adds INSERT statements for all rows. "Export as CSV…" saves all rows. Export needs the desktop app.
  - Refresh reloads the table list.
  - "Truncate…" and "Drop table…" ask you to confirm first. On a production connection, you must also select a checkbox. The statement commits immediately and goes into the audit log. A read-only connection cannot use these items.
- The database list in the editor bar now opens to the right. Before, the sidebar hid its left part.

## [0.1.2] - 2026-11-03

- You can now delete the last connection. When no connection remains, sandb shows the "No connections yet" screen.

## [0.1.1] - 2026-11-03

- Test release for the auto-update flow. No functional changes.

## [0.1.0] - 2026-11-03

First release.

- Connect to PostgreSQL, MySQL, MariaDB and SQLite.
- SQL editor with autocomplete, query tabs and an editable results grid.
- AI chat with any OpenAI-compatible provider, with read-only and write-approval safeguards.
- Saved passwords in an encrypted file in the app data folder.
- Version in Settings, with a check for updates that asks before it installs.
