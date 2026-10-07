# Explicit event-count paths

The approved count_paths contract lives in ti.toml:

```toml
[ingest]
count_paths = ['electrical.bilge.pumpCycles']
```

Both the section and its keys reject unknown fields. The list contains unique,
exact numeric leaf paths, without globs or aggregate suffixes. An object parent
does not select its children. String, boolean, null, position and non-finite
values are ignored for listed event paths. Normal object flattening can produce
eligible numeric leaves, but each leaf must be listed explicitly.

For each entity and closed bucket, the bare column counts finite numeric events
from the preferred source, independently of their magnitudes. Configured source
priorities win; absent priorities, the first source seen wins. Equal-priority
sources retain that first source. A newly seen higher-priority source replaces the
lower-priority count. Equal timestamps remain separate events. Existing replay
idempotency is unchanged. There is no edge detection or cumulative-counter delta.

The catalog registers a separate FieldKind::Count with no aggregate: unsigned
scale-zero bit slices, nullable UInt64 Arrow, PostgreSQL bigint (OID 20).
Ordinary and listed numeric paths retain value-aggregate semantics. Every finite
listed sample increments the bare preferred-source count and opt-in @count.
Representable samples retain @mean/@min/@max/@last; magnitudes outside the
fixed-point BSI range contribute only to counts. Each bucket persists the number
excluded as path@skipped_magnitudes, a nullable count column. Ingest status reports
skipped_magnitudes.total and skipped_magnitudes.paths, summed over the default
store's closed buckets. Non-finite samples contribute to neither accumulator.
Historical @mean/@min/@max/@last columns retain their original stored values. The explicit bare count overrides the usual mean
alias. Missing event samples yield NULL, never fabricated zero.

## Configuration changes and existing history

Each BucketWindow snapshots the list on its first sample. Updating the list
affects newly opened buckets; closing an already-open bucket never applies the
new list retroactively. The snapshot is serialized in the existing backfill
window journal, so journal reloads preserve the policy.

Live late events reuse the original bucket snapshot and accumulator in a bounded
128-window repair cache. Older late-event buckets fail explicitly and require
historical backfill, rather than overwriting a stored count with one. Empty-policy
buckets retain their original numeric interpretation on a late rewrite.

Existing sealed shards and field IDs are unchanged. Enabling a path registers a
new bare count field; older numeric values remain under path@mean, and the new
bare column is NULL in buckets without a stored count. Removing a path stops
writing counts to newly opened buckets; stored counts remain queryable, while
new numeric values remain under their explicit aggregate names. Once a catalog
contains an explicit bare count, it remains the canonical bare column.
Historical backfill with a different list is an explicit rewrite; it is not
triggered by a read or a configuration edit. Rebuild a separate store to change
historical semantics intentionally.

The default list is empty. It adds no catalog fields or bitmap records, and does
not alter the canonical seal encoding.
