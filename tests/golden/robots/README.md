# W9 robot-fleet golden corpus

Three robots, two UTC days, one sample/second in six wide Parquet files; six incident documents; and a six-bucket Signal K boat fixture. Generate with `ti-bench gen --profile robots --root <data>`. Import the boat with `lume ti backfill --signalk <data>/signalk/tier=raw --store <store>`, then the wide robot files with `--entity robot_id --time timestamp --time-unit ms --wide --prefix robot. --units <data>/units.toml`. Import incidents with `lume ti import-docs --parquet <data>/documents/incidents.parquet --entity entity --time start_ms --time-end end_ms --time-unit ms --id id --kind kind --title title --body body --store <store>`.

The 14 entries cover point lookup, bucket counts, numeric and single-valued set predicates, source membership, daily rollups, intervals, pose, mapped document search/ranges/joins, and mixed entity grouping. Every oracle must return a nonempty result. The initial stored outputs follow the generator's analytical invariants; independent DuckDB verification is a required host acceptance step.

On a host with Python DuckDB and built TI executables:

```
python tests/golden/robots/oracle.py --data-dir <data> --store <store> --prepare
```

The script reads the same raw Parquet directly, constructs quantized ten-second scalar bucket views, compares all stored outputs, and runs `lume ti verify --store <store> --corpus tests/golden/robots`. `--write-expected` refreshes outputs from DuckDB. DuckDB uses two threads, 512 MiB of memory, and at most 4 GiB of project-local scratch. It is a host test dependency only.

Robot temperature deliberately lacks a units entry; `ti status` must report `robot.temperature` under unit/scale misses. Units for battery voltage, motor current, and pose are explicit. The independent Signal K fleet/hash regression remains a separate acceptance gate.
