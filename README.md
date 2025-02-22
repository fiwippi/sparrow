## sparrow

Audio visualiser for DMX lights.

**Step 1:** Run sparrow

```sh
> cargo build && ./target/debug/sparrow --help
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.08s
Usage: sparrow [OPTIONS]

Options:
      --log-level <LOG_LEVEL>      [default: INFO] [possible values: CRITICAL, ERROR, WARN, INFO, DEBUG, TRACE]
  -b, --buffer-size <BUFFER_SIZE>  [default: xl] [possible values: xs, s, m, l, xl]
  -p, --min-period <MIN_PERIOD>    [default: 250]
  -h, --help                       Print help
  -V, --version                    Print version
```

**Step 2:** Visit the UI, at `localhost:4181`
