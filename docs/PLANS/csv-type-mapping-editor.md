# CSV type mapping editor

The CSV preview owns one SQL type input per source column. Each starts with the
type inferred for the selected destination engine. A blank input restores the
inferred type. Nonblank values are validated with the same engine dialect and
single-data-type rule used by the server before generating SQL or saving a
recipe. The server remains authoritative when an import executes.

For a new table, the preview opens a reviewable CREATE TABLE query with the
selected types. For an existing table, importing rows sends the selected
mapping to the CSV API. The preview can also populate a CSV import transfer
recipe, including its type mappings, table, conflict policy, and create-table
choice. Durable resume remains incompatible with explicit mappings.
