# SQL editor dialect feature matrix

`Complete` means Sift returns an exact, revision-bound answer for the stated
scope. `Partial` means the UI exposes only proven subsets and labels unresolved
or dynamic shapes as uncertain. `Unsupported` means Sift fails closed instead
of synthesizing metadata.

| Semantic feature | PostgreSQL | SQL Server | Exact scope and fail-closed boundary |
|---|---|---|---|
| Catalog relations, aliases, columns | Complete | Complete | Selected database/catalog revision with complete object metadata. |
| CTE and derived-table projections | Complete | Complete | Explicit aliases or projections made only of named expressions. Dynamic/unnamed expressions do not expand. |
| Multi-relation `*` | Complete | Complete | Catalog-complete query relations; output is qualified in relation order. |
| `JOIN ... USING` / natural-join `*` | Complete | Complete | Proven ordered columns only. Coalesced join columns appear once; missing `USING` owners fail closed. |
| Document temp tables | Complete | Complete | PostgreSQL `CREATE TEMP[TEMPORARY] TABLE`; SQL Server local `#table` declarations accepted by the bundled parser. Runtime-created/dynamic temp shapes are unsupported. |
| Table-valued functions | Complete | Complete | Catalog-reported, ordered result columns. Dynamic or polymorphic result records are unsupported. |
| Composite record fields | Complete | Unsupported | PostgreSQL native composite column type resolved to a complete catalog `Type` node. Anonymous `record` and dynamic field sets fail closed. |
| `inserted` / `deleted` pseudo tables | Unsupported | Complete | SQL Server DML `OUTPUT` and statically targeted trigger bodies; columns come from the exact target object. |
| Routine overload completion | Complete | Complete | Every catalog signature remains a distinct candidate. |
| Routine overload hover selection | Partial | Partial | Exact when arity selects one visible signature; same-arity overloads remain explicitly unresolved. |
| Expression type inference | Partial | Partial | Direct catalog columns, composite fields, routine returns, and native/portable type metadata. Dynamic SQL and unknown procedure result shapes are unsupported. |

Quoted identifiers preserve engine rules (`"name"` for PostgreSQL and
`[name]` for SQL Server). Every prepared star edit is tied to the document and
catalog revisions and is emitted with `exact: true` only after all participating
relations and ordered columns resolve.
