# ft_lock

To disable the need of sudo when launching it to disable the keys do this :
1. Create a udev rule:
```bash
sudo tee /etc/udev/rules.d/99-ft_lock.rules > /dev/null <<'EOF'
KERNEL=="uinput", MODE="660", GROUP="input"
KERNEL=="event*", SUBSYSTEM=="input", MODE="660", GROUP="input"
EOF
```
2. Reload rules and trigger devices:
```bash
sudo udevadm control --reload-rules
sudo udevadm trigger
```
3. Add your user to the ```input``` group:
```bash
sudo getent group input || sudo groupadd input
sudo usermod -aG input "$USER"
```
4. Load uinput if needed:
```bash
sudo modprobe uinput
echo uinput | sudo tee /etc/modules-load.d/uinput.conf >/dev/null
```
5. Log out and log back in (or run newgrp input) for group change to take effect.
