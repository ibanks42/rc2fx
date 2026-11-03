# Hardware

These figures were read from a running RØDECaster Pro II.

| | |
| --- | --- |
| Processor | Four ARM Cortex-A53 cores (ARMv8-A, AArch64), Amlogic Meson AXG family. The exact SoC part number is unconfirmed. |
| Clock | 1.416 GHz on all four cores with the performance governor. This is the configured clock; the silicon may allow more. |
| SIMD | NEON (`asimd`). There are no dot-product or SVE extensions. |
| Memory | 512 MiB installed, about 459 MiB visible to Linux, typically 180–195 MiB free while running. No swap. The device tree in the firmware says 256 MiB, which is wrong. |
| Cache | One L2 cache shared by all four cores. Memory bandwidth is the tightest resource: a large working set on any core can slow the audio engine. |
| OS | Linux 5.10.17 with PREEMPT_RT, glibc 2.35, sysvinit, A/B root partitions. |
| Audio | JACK at 48 kHz with 112-sample periods (2.33 ms). This is the processing cycle. Total hardware latency is higher. |

## Core usage

RØDE's audio work runs on CPUs 1–3, and the main audio thread is pinned to
CPU 2. RØDE's helper and screen threads also run on those cores, so they are
busy. RC2FX's suppression worker runs on CPU 0, and the launcher moves RØDE's
CPU-0 housekeeping threads to other cores.

## Quirks

- After `/sbin/reboot` the mixer sometimes stays dark until it is
  power-cycled.
- On the first boot after an update, the stock launcher unloads the audio
  drivers, reflashes the Bluetooth, digital and fader controllers, removes
  `/usr/bin/first-boot` and restarts. Leave the mixer alone until it
  finishes.
- The stock engine prints the Wi-Fi password to its output at start-up.
  RC2FX's tooling removes it from anything it displays or saves.
