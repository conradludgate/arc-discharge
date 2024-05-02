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

<details><summary>Requests/sec: 140462.7993</summary>

```
Summary:
  Success rate:	100.00%
  Total:	14.2386 secs
  Slowest:	0.0150 secs
  Fastest:	0.0000 secs
  Average:	0.0009 secs
  Requests/sec:	140462.7993

  Total data:	51.50 MiB
  Size/request:	27 B
  Size/sec:	3.62 MiB

Response time histogram:
  0.000 [1]       |
  0.002 [1979204] |■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■
  0.003 [19933]   |
  0.005 [203]     |
  0.006 [45]      |
  0.008 [90]      |
  0.009 [12]      |
  0.011 [11]      |
  0.012 [334]     |
  0.014 [39]      |
  0.015 [128]     |

Response time distribution:
  10.00% in 0.0007 secs
  25.00% in 0.0008 secs
  50.00% in 0.0009 secs
  75.00% in 0.0010 secs
  90.00% in 0.0011 secs
  95.00% in 0.0012 secs
  99.00% in 0.0016 secs
  99.90% in 0.0023 secs
  99.99% in 0.0119 secs


Details (average, fastest, slowest):
  DNS+dialup:	0.0046 secs, 0.0033 secs, 0.0064 secs
  DNS-lookup:	0.0001 secs, 0.0000 secs, 0.0009 secs

Status code distribution:
  [200] 2000000 responses
```
</details>

#### arc-discharge

<details><summary>Requests/sec:	137845.6894</summary>

```
Summary:
  Success rate:	100.00%
  Total:	14.5090 secs
  Slowest:	0.0241 secs
  Fastest:	0.0000 secs
  Average:	0.0009 secs
  Requests/sec:	137845.6894

  Total data:	51.50 MiB
  Size/request:	27 B
  Size/sec:	3.55 MiB

Response time histogram:
  0.000 [1]       |
  0.002 [1986012] |■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■
  0.005 [10024]   |
  0.007 [2308]    |
  0.010 [748]     |
  0.012 [410]     |
  0.014 [252]     |
  0.017 [176]     |
  0.019 [16]      |
  0.022 [18]      |
  0.024 [34]      |

Response time distribution:
  10.00% in 0.0006 secs
  25.00% in 0.0007 secs
  50.00% in 0.0008 secs
  75.00% in 0.0010 secs
  90.00% in 0.0013 secs
  95.00% in 0.0015 secs
  99.00% in 0.0021 secs
  99.90% in 0.0067 secs
  99.99% in 0.0150 secs


Details (average, fastest, slowest):
  DNS+dialup:	0.0031 secs, 0.0008 secs, 0.0043 secs
  DNS-lookup:	0.0001 secs, 0.0000 secs, 0.0008 secs

Status code distribution:
  [200] 1999999 responses

Error distribution:
  [1] connection error
```
</details>

#### Results

tokio has a 3.4% throughput advantage.

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
 Rust                   16         2711         2054          176          481
 |- Markdown            14         1678            9         1264          405
 (Total)                           4389         2063         1440          886
===============================================================================
 Total                  16         2711         2054          176          481
===============================================================================

# unsafe
1023 matches
970 matched lines
147 files contained matches
```

#### Results

arc-discharge has only 5% of the code that tokio has, and 1/8th of the total unsafe code.
