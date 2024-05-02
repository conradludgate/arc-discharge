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

<details><summary>Requests/sec: 139769.7159</summary>

```
Summary:
  Success rate:	100.00%
  Total:	14.3093 secs
  Slowest:	0.0116 secs
  Fastest:	0.0000 secs
  Average:	0.0009 secs
  Requests/sec:	139769.7159

  Total data:	51.50 MiB
  Size/request:	27 B
  Size/sec:	3.60 MiB

Response time histogram:
  0.000 [1]       |
  0.001 [1847735] |■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■
  0.002 [148770]  |■■
  0.003 [2616]    |
  0.005 [373]     |
  0.006 [93]      |
  0.007 [146]     |
  0.008 [6]       |
  0.009 [3]       |
  0.010 [35]      |
  0.012 [222]     |

Response time distribution:
  10.00% in 0.0007 secs
  25.00% in 0.0008 secs
  50.00% in 0.0009 secs
  75.00% in 0.0010 secs
  90.00% in 0.0011 secs
  95.00% in 0.0013 secs
  99.00% in 0.0016 secs
  99.90% in 0.0027 secs
  99.99% in 0.0106 secs


Details (average, fastest, slowest):
  DNS+dialup:	0.0030 secs, 0.0024 secs, 0.0043 secs
  DNS-lookup:	0.0001 secs, 0.0000 secs, 0.0007 secs

Status code distribution:
  [200] 2000000 responses
```
</details>

#### arc-discharge

<details><summary>Requests/sec:	141106.1541</summary>

```
Summary:
  Success rate:	100.00%
  Total:	14.1737 secs
  Slowest:	0.0156 secs
  Fastest:	0.0000 secs
  Average:	0.0009 secs
  Requests/sec:	141106.1541

  Total data:	51.50 MiB
  Size/request:	27 B
  Size/sec:	3.63 MiB

Response time histogram:
  0.000 [1]       |
  0.002 [1952169] |■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■
  0.003 [45906]   |
  0.005 [854]     |
  0.006 [352]     |
  0.008 [56]      |
  0.009 [22]      |
  0.011 [240]     |
  0.012 [142]     |
  0.014 [55]      |
  0.016 [203]     |

Response time distribution:
  10.00% in 0.0006 secs
  25.00% in 0.0007 secs
  50.00% in 0.0008 secs
  75.00% in 0.0010 secs
  90.00% in 0.0013 secs
  95.00% in 0.0014 secs
  99.00% in 0.0018 secs
  99.90% in 0.0031 secs
  99.99% in 0.0141 secs


Details (average, fastest, slowest):
  DNS+dialup:	0.0034 secs, 0.0025 secs, 0.0058 secs
  DNS-lookup:	0.0001 secs, 0.0000 secs, 0.0015 secs

Status code distribution:
  [200] 2000000 responses
```
</details>

#### Results

`arc-discharge` has a 1.0% throughput advantage, but slightly worse tail latencies

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
