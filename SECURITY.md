# Security

LetShare turns a Windows laptop into a Wi-Fi access point and routes that
traffic to the internet. That is a privileged, network-facing job, so treat it
like one.

## Reporting a vulnerability

Use GitHub's **private vulnerability reporting** on this repository
(Security -> Report a vulnerability). Please do not open a public issue for
anything that could let someone on the LAN escalate, poison DNS, intercept
traffic, or persist on the host.

Include the version, Windows build, adapter, and the smallest reproduction you
can manage.

## What a client on the AP can do

- It joins a normal WPA2 network and receives a lease, a gateway and a resolver.
- Its traffic is translated by WinNat to the laptop's uplink.
- **There is no client isolation.** Clients can reach each other and can reach
  services listening on the laptop. Windows Firewall still applies to the
  laptop's own ports and its profile is whatever the AP adapter is classified
  as, but this project creates no firewall rules of its own.
- Anyone who knows the passphrase can join. **The passphrase in the source tree
  is a published placeholder.** Set `LETSSHARE_PASSPHRASE` before relying on
  this, and use a network you control.

## Known limitations, stated plainly

- **The supervisor runs elevated** from a logon scheduled task. Binaries are
  installed under `%ProgramFiles%\LetShare`, which non-administrators cannot
  write, so replacing them requires administrator rights.
- **The control channel is unauthenticated.** `letsshare-tier2 restart ...`
  drops a command file in the user's temp directory and the elevated supervisor
  acts on it. Any process running as that user can therefore restart
  components. It cannot run arbitrary commands through this path, but it is a
  denial-of-service surface.
- **Logs are plain text in `%TEMP%`** and contain client MAC addresses, lease
  addresses and every DNS name a client resolved. They are readable by the
  logged-in user. Delete them if that matters to you.
- **DNS and DHCP are hardened but not audited.** The forwarder uses a fresh
  socket, its own transaction id and a connected socket per query, rejects
  replies that do not echo the question, and caches only rcode 0 answers. DHCP
  expires leases and evicts the oldest when the pool is full. Neither has had a
  formal security review.
- **WinNat is host-global.** Hyper-V, WSL and Docker also use it. LetShare
  creates an instance named `LetShare` for `192.168.50.0/24` and removes it on
  uninstall; another tool claiming the same name or prefix will conflict.
- **The AP is a single radio.** Uplink and clients share it, so throughput is
  roughly half the uplink rate. That is a physics limit, not a bug.

## What this project does not do

- It does not disable or reconfigure Internet Connection Sharing. ICS can still
  be enabled elsewhere and may re-create `192.168.137.1`.
- It does not manage firewall rules, client isolation, or IPv6.
- It does not protect clients from each other or from the host.
