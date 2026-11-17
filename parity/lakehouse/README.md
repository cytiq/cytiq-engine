# Lakehouse interop

`interop.py` checks that `cytiq export events` tables are read exactly by the
standard tools a lakehouse uses: PyArrow and DuckDB. The pinned versions are
in `requirements.lock` (hashes from `uv pip compile --generate-hashes`).
PyArrow has the same version as in `parity/requirements.txt`.

```bash
just lakehouse-setup                       # target/lakehouse/py from the lock
just lakehouse-interop                     # one million events in the large case
just lakehouse-interop --events 10000000   # the weekly run
```

The recipe builds the release `cytiq` and the `events_layouts` example, which
generates the inputs. The report is written to `target/lakehouse/interop/interop.json`.
`.github/workflows/lakehouse.yml` runs the check on pull requests that change
the export or its readers, and weekly. It is not a required check.

## What is checked

The oracle is the script's own FCS DATA reader, not cytiq. It slices each
channel out of the row-major DATA segment.

Inputs:
- float32, float64 and mixed-type files whose first rows hold `-0.0`, NaN
  payloads, infinities, subnormals, the largest finite values and integers
  above 2^53 and 2^63;
- one event;
- one million events;
- `--events` events of 30 mixed-type channels;
- unusual channel names.

Each input is exported as one table and as Snappy parts, and checked:
- **PyArrow:** every channel's bytes equal the FCS DATA bytes, bit for bit.
  Integers are compared widened to 64 bits. `source_event_id` is `0..n` in
  order, and every row names the acquisition and one layer.
- **DuckDB:**
  - `UBIGINT`/`FLOAT`/`DOUBLE` column types;
  - every event counted once;
  - quoted unusual names;
  - the channel dictionary through `parquet_kv_metadata`;
  - -0.0, NaN, infinities and integers above 2^63 survive SQL;
  - parts read through a glob hold exactly the single table's values,
    compared by count and summed value hashes rather than floating sums,
    which differ in the last bits with the grouping.
- **Bytes:** exporting twice gives the same bytes, and every output's SHA-256
  matches the provenance record.
- **Refusals:** garbage, truncated and zero-event FCS files exit 2 and leave
  no table, part, record or staged file.

A zero-event FCS file has no DATA segment, and `cytiq-fcs` requires one, so
export refuses it.
