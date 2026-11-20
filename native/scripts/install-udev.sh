#!/usr/bin/env bash
# Raw HID access for the tablets + keep them away from libinput, so the pen
# moves only the game marker and never the system cursor.
# Run once:  sudo scripts/install-udev.sh
set -euo pipefail
rule=/etc/udev/rules.d/70-deemo-tablets.rules
sudo tee "$rule" >/dev/null <<'RULES'
# Deemo: hidraw access for the seat user (Gaomon 256c, Parblo 0483:a019,
# XP-Pen Star G640/G640S 28bd:0094,0906,0914).
KERNEL=="hidraw*", ATTRS{idVendor}=="256c", TAG+="uaccess", TAG+="udev-acl"
KERNEL=="hidraw*", ATTRS{idVendor}=="0483", ATTRS{idProduct}=="a019", TAG+="uaccess", TAG+="udev-acl"
KERNEL=="hidraw*", ATTRS{idVendor}=="28bd", ATTRS{idProduct}=="0094", TAG+="uaccess", TAG+="udev-acl"
KERNEL=="hidraw*", ATTRS{idVendor}=="28bd", ATTRS{idProduct}=="0906", TAG+="uaccess", TAG+="udev-acl"
KERNEL=="hidraw*", ATTRS{idVendor}=="28bd", ATTRS{idProduct}=="0914", TAG+="uaccess", TAG+="udev-acl"
# The compositor must not see the pen: no system cursor movement.
SUBSYSTEM=="input", ATTRS{idVendor}=="256c", ENV{LIBINPUT_IGNORE_DEVICE}="1"
SUBSYSTEM=="input", ATTRS{idVendor}=="0483", ATTRS{idProduct}=="a019", ENV{LIBINPUT_IGNORE_DEVICE}="1"
SUBSYSTEM=="input", ATTRS{idVendor}=="28bd", ATTRS{idProduct}=="0094", ENV{LIBINPUT_IGNORE_DEVICE}="1"
SUBSYSTEM=="input", ATTRS{idVendor}=="28bd", ATTRS{idProduct}=="0906", ENV{LIBINPUT_IGNORE_DEVICE}="1"
SUBSYSTEM=="input", ATTRS{idVendor}=="28bd", ATTRS{idProduct}=="0914", ENV{LIBINPUT_IGNORE_DEVICE}="1"
RULES
sudo udevadm control --reload-rules
sudo udevadm trigger --subsystem-match=hidraw
sudo udevadm trigger --subsystem-match=input
echo "installed $rule"
echo "replug the tablet (or re-login) and restart the game"
