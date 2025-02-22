## sparrow

Audio visualiser for DMX lights.

**Step 1:** Build an run sparrow

```sh
> cargo build && ./target/debug/sparrow --help
Usage: sparrow [OPTIONS]

Options:
      --log-level <LOG_LEVEL>
          [default: INFO] [possible values: CRITICAL, ERROR, WARN, INFO, DEBUG, TRACE]
  -b, --buffer-size <BUFFER_SIZE>
          The size of the buffer used to calculate the final LED colour,
          smaller values are quicker to process at the cost of reduced
          accuracy [default: xl] [possible values: xs, s, m, l, xl]
  -p, --interpolation-period <INTERPOLATION_PERIOD>
          How long a colour is interpolated before a new measurement is
          taken, smaller values lead to rougher colour changes [default: 250]
  -h, --help
          Print help
  -V, --version
          Print version

> ./target/debug/sparrow
Feb 22 12:10:15.959 INFO Listening on 0.0.0.0:4181...
```

**Step 2:** Visit the UI, at `localhost:4181`
