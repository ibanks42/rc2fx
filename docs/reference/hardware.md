# Hardware

Everything below was measured directly on a running RØDECaster Pro II.

| | |
| --- | --- |
| Processor | Quad-core ARM Cortex-A53 (ARMv8-A, AArch64), Amlogic Meson AXG family. Exact SoC part number isn't confirmed. |
| Clock | 1.416 GHz across all four cores under the `performance` governor. That's what's configured; the silicon might be capable of more. |
| SIMD | NEON (`asimd`). No dot-product or SVE extensions. |
| Memory | 512 MiB installed, ~459 MiB visible to Linux, usually 180–195 MiB free at runtime, with no swap. The firmware's device tree claims 256 MiB, which is wrong. |
| Cache | A single L2 cache shared across all four cores. Memory bandwidth is the real bottleneck here, since a big working set on any core can starve the audio engine. |
| OS | Linux 5.10.17 with PREEMPT_RT, glibc 2.35, sysvinit, and A/B root partitions. |
| Audio | JACK at 48 kHz with 112-sample periods (2.33 ms). That's just the processing cycle; total hardware latency is higher. |

## Core usage

RØDE's audio code runs on CPUs 1–3, with the main audio thread pinned to CPU 2. Between that, helper threads, and the UI, those three cores stay busy. RC2FX puts its noise suppression worker on CPU 0, and the launcher moves RØDE's CPU-0 housekeeping threads off onto other cores to keep it clear.

## Quirks

- Running `/sbin/reboot` occasionally leaves the mixer dark until you pull power and plug it back in.
- On the first boot after a firmware update, RØDE's stock launcher unloads the audio drivers, reflashes the Bluetooth, digital, and fader controllers, deletes `/usr/bin/first-boot`, and reboots. Don't touch the mixer while it's doing this.
- The stock engine logs the Wi-Fi password on startup. RC2FX's tools strip it out before displaying or saving any logs.
