# Why it is this fast, and what would actually make it faster

Measured on 2026-10-23, test-laptop, Intel AX203.

## The numbers that matter

| Measurement | Value |
|---|---|
| Router BSSID | `the router`, **802.11n**, **2.4 GHz**, channel 1 |
| Signal to router | 60% (roughly -70 dBm) |
| Laptop receive rate | 39 Mbps |
| Laptop **transmit** rate | **14.4 Mbps** |
| Internet download measured from the laptop | **14.41 Mbps** |
| AP adapter nominal link | 29 Mbps |
| Stack CPU cost | ~3 s total across all processes, hours of uptime |

The measured internet speed equals the laptop's transmit rate almost exactly.
The Wi-Fi link to the router is saturated; the ISP is not the limit, and neither
is any of our software.

## Why the phone gets roughly half

The laptop has one radio. It is simultaneously:

- a station (STA) associated with the router, carrying the uplink, and
- an access point (AP) serving the phone.

Every packet crosses that radio twice: phone -> laptop, then laptop -> router.
A single radio cannot receive and transmit on the same channel at the same time,
so the two roles time-share the medium. That halves the throughput before any
protocol overhead.

The AP also inherits the STA's channel. Because the uplink is channel 1 on
2.4 GHz, the phone's AP is on 2.4 GHz channel 1 too.

## What does not work: amplifying or repeating

Shannon-Hartley sets the ceiling:

```
C = B * log2(1 + S/N)
```

Capacity depends on bandwidth `B` and signal-to-noise ratio `S/N`.

- An amplifier raises the signal, but it raises noise too. What a receiver can
  use is SNR, not absolute power. Once the noise floor or the regulatory EIRP
  limit (20 dBm on 2.4 GHz) is reached, more power buys nothing.
- The link is asymmetric. Your phone's transmit power and antenna are the weak
  side of the return path; amplifying the laptop does not help the phone talk.
- A repeater is store-and-forward: it must receive the whole frame before it can
  retransmit it. On the same channel that is two uses of the medium for one
  frame, so a repeater typically **halves** throughput. It extends range; it
  cannot create rate.
- A dual-radio repeater avoids the halving, but its backhaul is still capped by
  its own link rate. Relaying cannot create information that was never received.

Research directions (reconfigurable intelligent surfaces, in-band full duplex,
cell-free massive MIMO, Wi-Fi 7 multi-link operation) improve coverage or
spectral efficiency. None of them lets a relay exceed the capacity of its source
link, and none are in consumer hardware you can enable today.

## What actually works, ranked by gain per rupee

1. **Plug the laptop into the router with Ethernet.** The laptop has a Realtek
   Gigabit port, currently disconnected. The uplink becomes 1 Gbps wired, the
   radio is freed for the AP alone, and the time-sharing penalty disappears.
   Expected gain: several times, limited by the AP's own link rate. Cost: one
   cable.
2. **Give the AP its own radio.** A USB Wi-Fi adapter dedicated to the uplink
   (or to the AP) removes the same 50% penalty wirelessly. Cost: a cheap dongle.
3. **Get the uplink onto 5 GHz.** The router broadcasts only a 2.4 GHz BSSID.
   Enabling its 5 GHz radio, or using a 5 GHz AP, raises the link rate by an
   order of magnitude at the same distance.
4. **Move closer or improve line of sight.** At 60% signal the link negotiates a
   low MCS. Better SNR can double or triple the rate with no hardware change.

Changing the router's channel is not useful here: only one AP is visible on
channel 1, so it is not congested.

## Software changes made

- DNS now caches responses for their TTL, with a 5 s floor and 5 min ceiling,
  keyed case-insensitively and per record type. Repeat lookups never leave the
  machine. This improves page-load latency, not throughput.
- The supervisor no longer spawns `netstat` or PowerShell on every tick; port
  state is a bind syscall and the NAT flag is carried from the 30 s maintenance
  pass. This removed both the console flashing and a per-tick cost.
