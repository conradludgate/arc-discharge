# arc-discharge

Lightning fast and simple async runtime.

This is intended to be an efficient async runtime with readable code and only a small amount of unsafe.
It does depend on some libraries with lots of unsafe and unstable features. But the concepts of those libraries
are abstracted away from the concept of the runtime.

## Benchmarks

### HTTP

Using [oha](https://github.com/hatoo/oha) to load test a [demo HTTP app](https://github.com/tokio-rs/tokio/blob/fc69666f8aaa788beaaf091ce6a9abd7b03d5e27/examples/tinyhttp.rs) with both tokio and arc-discharge as the runtimes we get these results

#### tokio

<details><summary>Requests/sec: 154639.7143</summary>

```
Summary:
  Success rate: 100.00%
  Total:        64.6664 secs
  Slowest:      0.1320 secs
  Fastest:      0.0000 secs
  Average:      0.0012 secs
  Requests/sec: 154639.7143

  Total data:   257.49 MiB
  Size/request: 27 B
  Size/sec:	    3.98 MiB

Response time histogram:
  0.000 [1]       |
  0.013 [9999909] |■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■
  0.026 [0]       |
  0.040 [0]       |
  0.053 [0]       |
  0.066 [6]       |
  0.079 [4]       |
  0.092 [11]      |
  0.106 [5]       |
  0.119 [0]       |
  0.132 [1]       |

Response time distribution:
  10% in 0.0009 secs
  25% in 0.0011 secs
  50% in 0.0012 secs
  75% in 0.0014 secs
  90% in 0.0016 secs
  95% in 0.0019 secs
  99% in 0.0023 secs

Status code distribution:
  [200] 9999937 responses

Error distribution:
  [63] connection error: Connection reset by peer (os error 54)
```
</details>

#### arc-discharge

<details><summary>Requests/sec:	149558.2923</summary>

```
Summary:
  Success rate: 100.00%
  Total:        66.8636 secs
  Slowest:      0.1409 secs
  Fastest:      0.0000 secs
  Average:      0.0013 secs
  Requests/sec: 149558.2923

  Total data:   257.49 MiB
  Size/request: 27 B
  Size/sec:     3.85 MiB

Response time histogram:
  0.000 [1]       |
  0.014 [9999927] |■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■
  0.028 [0]       |
  0.042 [0]       |
  0.056 [0]       |
  0.070 [4]       |
  0.085 [3]       |
  0.099 [1]       |
  0.113 [1]       |
  0.127 [0]       |
  0.141 [1]       |

Response time distribution:
  10% in 0.0009 secs
  25% in 0.0010 secs
  50% in 0.0012 secs
  75% in 0.0015 secs
  90% in 0.0019 secs
  95% in 0.0021 secs
  99% in 0.0024 secs

Status code distribution:
  [200] 9999938 responses

Error distribution:
  [62] connection error: Connection reset by peer (os error 54)
```
</details>

#### Results

tokio has a 3.4% throughput advantage.

### Code complexity

Using [tokei](https://github.com/XAMPPRocky/tokei), we measure the lines of code of each project.
Using `ripgrep`, we search for uses of unsafe inside the source and vendored dependencies (excluding windows-api/libc)

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
10041 matches
9863 matched lines
1052 files contained matches
```

#### arc-discharge

```
===============================================================================
 Language            Files        Lines         Code     Comments       Blanks
===============================================================================
 Rust                   16         2800         2121          180          499
 |- Markdown            14         1829            9         1385          435
 (Total)                           4629         2130         1565          934
===============================================================================
 Total                  16         2800         2121          180          499
===============================================================================

# unsafe
3133 matches
3067 matched lines
474 files contained matches
```

#### Results

arc-discharge has only 5% of the code that tokio has, and only a third of the unsafe code.
