# Relations redesign: plan (temporary, delete when done)

## Goal
Relations feel like fields. Set one by inserting a value, read across edges
inside queries, and let the type system decide which mutable access is
sound. One API for fragmenting and non-fragmenting storage.

## User-facing API

```rust
struct Targets;                      // data-less
impl Relation for Targets { type Kind = OneToMany; }
struct DockedAt { berth: u8 }        // edge data = the relation value
impl Relation for DockedAt { type Kind = OneToMany; }

commands.spawn((Name("Raider"), Targets.to(station)));   // relate as a bundle part
commands.entity(raider).insert(Targets.to(hauler));      // retarget (one-target)
commands.entity(raider).remove::<Relates<Targets>>();    // unrelate
world.relate(ship, DockedAt { berth: 2 }, station);       // value-taking relate

Query<(&Name, To<Targets, &Hull>)>          // forward join
Query<(&Name, From<Targets, &Guns>)>        // backward join
Query<&Relates<DockedAt>>                   // edge data: Deref to DockedAt, .target()
q.each(|item| …)                            // closure iteration (always allowed)
let mut it = q.iter_lending(); while let Some(item) = it.fetch_next() { … }
q.cascade::<Follows>(|lead: Option<&Pos>, item| …)   // ACYCLIC relations only
```

## Semantics
- Item shape follows cardinality, at compile time:

  | | OneToOne | OneToMany | ManyToMany |
  |---|---|---|---|
  | `To<R, Q>` | `Option<Q::Item>` | `Option<Q::Item>` | iterator |
  | `From<R, Q>` | `Option<Q::Item>` | iterator | iterator |

  Joined entities that don't match `Q` are skipped (`None` / not yielded).
- `&mut` in joins: forbidden for ManyToMany (both directions).
- Edge data: `Relates<R>` stores the `R` value; `Deref<Target = R>`. Data-less
  relations are zero-sized. For ManyToMany it is one value per source.
- `R.to(target)` builds a pending `Relates<R>`; an `OnInsert<Relates<R>>` hook
  links it. Inserting over an existing link replaces it (the existing
  `OnReplace` hook unlinks first). For ManyToMany, `.to()` replaces all edges;
  `relate` adds one.
- Fragmenting relations are data-less (compile-time check): their target
  lives only in the table key, and edge data would need a second record of
  the link. `.to()` on a fragmenting relation inserts a pending `Relates<R>`
  that the hook turns into the pair and removes (extra moves; `relate` is
  still one move). `move_pair` marks the `PairOf<R>` tick on every retarget;
  the `Relinked<R>` filter reads the right column for either storage.
- Self-edges: refused at relate for relations with a one-side (they could
  alias a row's own `&mut T` with its join). Allowed for ManyToMany.

## Soundness model (compile time)
Every `QueryData` reports three access lists instead of one:
- `Access` (own): the row's entity.
- `Direct`: entities one hop away (first-level joins). Never the row itself
  (no self-edges).
- `Far`: entities two or more hops away (nested joins). May be anyone.

Validity (`FetchQuery::VALID`, every iteration form):
1. `Access` is valid (existing rule).
2. `Direct` is valid on its own: two first-level joins may hit the same entity.
3. `(Access, Far)`, `(Direct, Far)` and `Far` are valid: nested joins may hit
   the row or a direct target.
4. ManyToMany joins carry no writes.

`UNIQUE` (normal `Iterator` allowed, i.e. every `&mut` the pass yields is to a
distinct value):
- `Access` does not conflict with `(Direct, Far)` (across rows, a joined
  entity is another row), and
- `Far` has no writes, and
- no direct write goes through a shared path: `To` on OneToMany is shared;
  `From` on one-source relations and `To`/`From` on OneToOne are not.

Not `UNIQUE` means `iter_mut`/`for x in &mut q` fail to compile and point to
`each` / `iter_lending`. `get_mut`, `single`, `each` and `fetch_next` are
always fine (one item alive at a time). Joins have no contiguous form.

Scheduling: the query param's access becomes
`(Filtered<QueryAccess<Own, F>, F>, (Direct, Far))`. Join access is not
narrowed by the row filter. Inference sees joined components through `Uses`.

## Implementation steps
1. **Relates carries data, `.to()`, value-taking relate** (`relation.rs`).
   - `Relates<R> { side, value: R }`, `Deref/DerefMut`, `target()` kept.
   - `RelationExt::to` (blanket on `R: Relation`), pending entries `u32::MAX`.
   - `OnInsert<Relates<R>>` hook: queue `link_pending::<R>(entity)`.
   - `World::relate(source, value, target)`, `Commands::relate` same; self-edge
     check; migrate tests.
   - Fragmenting: `relation()` installs `Relates<R>` too; `link_pending` moves
     the pair and empties the side; `move_pair` touches the column tick.
2. **Query context** (`query.rs`): `QueryData::fetch(ctx, table, indices,
   tick)` with `ctx = Ctx { tables, locator, pairs }` so joins can reach
   other entities. Update every impl and caller (iter, get_mut, single,
   chunks, Single param).
3. **Access split**: `type Direct`, `type Far`, `const SHARED_WRITE`,
   `const JOINS`; tuples combine; `UNIQUE` const; param access in
   `param.rs`; validity in `FetchQuery::VALID`.
4. **`To<R, Q>` / `From<R, Q>`** (new `join.rs`): `JoinShape` on the
   cardinalities picks `Option` or `Joined<'w, Q>` iterator. Non-fragmenting
   reads `Relates<R>`/`Sources<R>` columns; fragmenting reads
   `table.targets` (target per table, resolved once per table) and the pair
   index (sources are whole tables).
5. **Iteration forms**: `Query::each`, `Query::iter_lending` +
   `QueryLending::fetch_next`; `iter_mut` asserts `UNIQUE`; contiguous asserts
   no joins.
6. **Clean cutover**: remove `Related<R>` filter's `iter_related`, `target`,
   `related_descendants` and `Query<&Sources<R>>::descendants` (replaced by
   `From`, `Tree`-free traversal via joins plus `World::descendants`
   iterator). `world.targets/sources` return iterators, not `Vec`.
7. **`ACYCLIC` + `cascade`**: `const ACYCLIC` on `Relation`; relate walks up
   from the target and refuses an edge that would close a cycle.
   `q.cascade::<R>(f)`: roots (no `Relates<R>` match), then breadth-first by
   sources; `f(Option<parent Q item>, row item)`. Lending form only.
8. **Docs, tests, Miri, benchmarks**: README relations section rewritten
   around the space model; tests for each rule (including compile-fail
   fixtures for `To<OneToMany, &mut>` + `iter_mut`, ManyToMany `&mut`, row
   aliasing through nested joins); allocation check for joins; paired
   benchmark (relation workload unchanged when no joins are used).

## Tests to add
- `.to()` spawn/insert/retarget/remove for both storages; edge data read and
  change detection on retarget (both storages).
- Join shapes per cardinality, `Q` mismatch skipping, despawned targets.
- `From<OneToMany, &mut T>` via normal iteration; `To<OneToMany, &mut T>` via
  `each` and `fetch_next`; compile-fail for `iter_mut` on it; compile-fail for
  ManyToMany `&mut`; compile-fail for two direct joins writing the same `T`.
- Self-edge refusal; ACYCLIC cycle refusal; `cascade` order.
- Fragmenting `To` resolves once per table (counted via a test-only probe is
  not allowed: assert behaviour instead, e.g. correctness after retarget).

## Risks
- `fetch(ctx, …)` touches every `QueryData` impl and the hot iteration path:
  keep `ctx` by reference, unused by plain components; check the paired
  benchmark for regressions.
- Specialization-heavy shape selection: keep `JoinShape` a plain trait on the
  three cardinality types (no specialization).
- Pending-link hook ordering with replace: verify with tests for insert over
  an existing link, and spawn + despawn in one flush.
