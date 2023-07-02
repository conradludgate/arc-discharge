# arc-discharge

Lightning fast and simple async runtime.

This is intended to be an efficient async runtime with readable code and only a small amount of unsafe.
It does depend on some libraries with lots of unsafe and unstable features. But the concepts of those libraries
are abstracted away from the concept of the runtime.

## Benchmarks

### HTTP

Using [oha](https://github.com/hatoo/oha) to load test a [demo HTTP app](https://github.com/tokio-rs/tokio/blob/fc69666f8aaa788beaaf091ce6a9abd7b03d5e27/examples/tinyhttp.rs) with both tokio and arc-discharge as the runtimes we get these results

#### tokio

```
Summary:
  Success rate:	100.00%
  Total:	6.4372 secs
  Slowest:	0.0068 secs
  Fastest:	0.0000 secs
  Average:	0.0008 secs
  Requests/sec:	155347.0222

  Total data:	25.75 MiB
  Size/request:	27 B
  Size/sec:	4.00 MiB

Response time histogram:
  0.000 [1]      |
  0.001 [310032] |■■■■■■■■■■■■■■
  0.001 [677040] |■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■
  0.002 [11492]  |
  0.003 [911]    |
  0.003 [267]    |
  0.004 [70]     |
  0.005 [58]     |
  0.005 [11]     |
  0.006 [27]     |
  0.007 [91]     |

Response time distribution:
  10% in 0.0006 secs
  25% in 0.0007 secs
  50% in 0.0008 secs
  75% in 0.0009 secs
  90% in 0.0011 secs
  95% in 0.0012 secs
  99% in 0.0014 secs
```

#### arc-discharge

```
Summary:
  Success rate:	100.00%
  Total:	6.1834 secs
  Slowest:	0.0062 secs
  Fastest:	0.0000 secs
  Average:	0.0008 secs
  Requests/sec:	161722.0705

  Total data:	25.75 MiB
  Size/request:	27 B
  Size/sec:	4.16 MiB

Response time histogram:
  0.000 [1]      |
  0.001 [320076] |■■■■■■■■■■■■■■■■
  0.001 [628038] |■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■
  0.002 [49366]  |■■
  0.003 [1837]   |
  0.003 [554]    |
  0.004 [0]      |
  0.004 [0]      |
  0.005 [0]      |
  0.006 [15]     |
  0.006 [113]    |

Response time distribution:
  10% in 0.0005 secs
  25% in 0.0006 secs
  50% in 0.0007 secs
  75% in 0.0009 secs
  90% in 0.0011 secs
  95% in 0.0013 secs
  99% in 0.0016 secs
```

#### Results

arc-discharge has a 4% throughput advantage.

### Code complexity

Using [tokei](https://github.com/XAMPPRocky/tokei), we measure the lines of code of each project, using `ripgrep`, we search for uses of unsafe
inside the source and vendored dependencies

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
3133 matches
3067 matched lines
474 files contained matches
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
10041 matches
9863 matched lines
1052 files contained matches
```

#### Results

arc-discharge has only 5% of the code that tokio has, and only a third of the unsafe code.
