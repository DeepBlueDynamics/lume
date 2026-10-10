# V4 generation cleanup

A successful v4 base publication runs best-effort cleanup. It keeps the current
generation and one previous published generation. Retired generations get a
10-minute grace period before removal. Entity batches do not trigger cleanup.

Preview or run cleanup:

```sh
lume index gc --db .lume-index --dry-run
lume index gc --db .lume-index --keep-generations 1 --gc-grace-secs 600
```

`--keep-generations` counts previous generations, from 0 to 100. The current
generation is always kept. `--gc-grace-secs 0` disables the grace period; use it
only when no other process is opening or writing this index. The default grace
reduces the chance of interrupting a reader opening an old generation, but does
not guarantee safety for a reader stalled longer than that period.

Unknown UUID directories are listed and retained. Use `--include-unknown` to
remove them, subject to the grace period measured from their directory mtime.
This includes failed or interrupted builds whose generation was never published.
Other names, root files, retention sidecars, symlinks and Windows reparse points
are never removed. Overlays are removed with their owning generation.

A loaded resident snapshot owns its data and keeps answering after its files
are removed. Cleanup may skip Windows sharing violations or other deletion
failures; it prints the path and retries on the next run. Cleanup failure never
changes the success of a publication.

Concurrent writers are unsupported. Do not run explicit cleanup alongside an
index writer. A corrupt pointer or retention history stops cleanup without
deleting generations. Cleanup is not a rollback command: retaining one previous
generation preserves its files; restoring its pointer remains a separate task.
