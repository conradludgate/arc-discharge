# arc-discharge

Lightning fast and simple async runtime.

This is intended to be an efficient async runtime with readable code and only a small amount of unsafe.
It does depend on some libraries with lots of unsafe and unstable features. But the concepts of those libraries
are abstracted away from the concept of the runtime.

## Disclaimer

This requires nightly APIs, is not well tested, and not recommended for production use. It is not meant to replace tokio.
The main purpose is to have similar performance characteristics to tokio with a much simpler code base for learning purposes.
It is not ensured to be satisfactory for soft real time systems.

## Benchmarks

### HTTP

Using [oha](https://github.com/hatoo/oha) to load test a [demo HTTP app](https://github.com/tokio-rs/tokio/blob/fc69666f8aaa788beaaf091ce6a9abd7b03d5e27/examples/tinyhttp.rs) with both tokio and arc-discharge as the runtimes we get these results

#### tokio

<details><summary>Requests/sec: 141882.2455</summary>

```
Summary:
  Success rate:	100.00%
  Total:	70.4810 secs
  Slowest:	0.0160 secs
  Fastest:	0.0000 secs
  Average:	0.0009 secs
  Requests/sec:	141882.2455

  Total data:	257.49 MiB
  Size/request:	27 B
  Size/sec:	3.65 MiB

Response time histogram:
  0.000 [1]       |
  0.002 [9929609] |■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■
  0.003 [68408]   |
  0.005 [1192]    |
  0.006 [225]     |
  0.008 [44]      |
  0.010 [44]      |
  0.011 [228]     |
  0.013 [211]     |
  0.014 [37]      |
  0.016 [1]       |

Response time distribution:
  10.00% in 0.0006 secs
  25.00% in 0.0008 secs
  50.00% in 0.0009 secs
  75.00% in 0.0010 secs
  90.00% in 0.0012 secs
  95.00% in 0.0013 secs
  99.00% in 0.0016 secs
  99.90% in 0.0020 secs
  99.99% in 0.0043 secs


Details (average, fastest, slowest):
  DNS+dialup:	0.0034 secs, 0.0024 secs, 0.0039 secs
  DNS-lookup:	0.0000 secs, 0.0000 secs, 0.0003 secs

Status code distribution:
  [200] 10000000 responses
```
</details>

#### arc-discharge

<details><summary>Requests/sec:	147177.0598</summary>

```
Summary:
  Success rate:	100.00%
  Total:	67.9454 secs
  Slowest:	0.0172 secs
  Fastest:	0.0000 secs
  Average:	0.0009 secs
  Requests/sec:	147177.0598

  Total data:	257.49 MiB
  Size/request:	27 B
  Size/sec:	3.79 MiB

Response time histogram:
  0.000 [1]       |
  0.002 [9900451] |■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■
  0.003 [98033]   |
  0.005 [772]     |
  0.007 [321]     |
  0.009 [29]      |
  0.010 [49]      |
  0.012 [16]      |
  0.014 [96]      |
  0.015 [94]      |
  0.017 [138]     |

Response time distribution:
  10.00% in 0.0006 secs
  25.00% in 0.0007 secs
  50.00% in 0.0008 secs
  75.00% in 0.0010 secs
  90.00% in 0.0012 secs
  95.00% in 0.0014 secs
  99.00% in 0.0017 secs
  99.90% in 0.0023 secs
  99.99% in 0.0044 secs


Details (average, fastest, slowest):
  DNS+dialup:	0.0031 secs, 0.0022 secs, 0.0035 secs
  DNS-lookup:	0.0000 secs, 0.0000 secs, 0.0003 secs

Status code distribution:
  [200] 10000000 responses
```
</details>

#### Results

`arc-discharge` has a 3.8% throughput advantage

### Code complexity

Using [tokei](https://github.com/XAMPPRocky/tokei), we measure the lines of code of each project.
Using `ripgrep`, we search for uses of unsafe inside the source and vendored dependencies (excluding windows-api/libc and dev dependencies)

#### tokio

```
===============================================================================
 Language            Files        Lines         Code     Comments       Blanks
===============================================================================
 Rust                  330        51477        39325         3372         8780
 |- Markdown           265        33613          963        24651         7999
 (Total)                          85090        40288        28023        16779
===============================================================================
 Total                 330        51477        39325         3372         8780
===============================================================================

# unsafe
8340 matches
8214 matched lines
860 files contained matches
```

#### arc-discharge

```
===============================================================================
 Language            Files        Lines         Code     Comments       Blanks
===============================================================================
 Rust                   17         2921         2235          172          514
 |- Markdown            15         1633            9         1234          390
 (Total)                           4554         2244         1406          904
===============================================================================
 Total                  17         2921         2235          172          514
===============================================================================

# unsafe
1023 matches
970 matched lines
147 files contained matches
```

#### Results

arc-discharge has only 5% of the code that tokio has, and 1/8th of the total unsafe code.
