# Omarchy Remote Launchpad

![Demo of Omarchy Remote Launchpad opening Linux apps in native macOS windows](launchpad-demo.gif)

![Omarchy Remote Launchpad with a remote ChatGPT window on macOS](screenshot.png)

Work with an Omarchy virtual machine or remote machine seamlessly from your Mac (broader support TBC)

## Goal

The main goal for this project is to meld an Omarchy machine with your current host. This means using applications installed on Omarchy as if it were native to MacOS or Windows

This means it is almost indistinguishable from your native applications, allowing you to get the best of both worlds with as little friction as possible

## Limitations

Currently the only supported *client* is OSX with 2 modes of running

- Remote: A preconfigured SSH enabled Omarchy instance (and sufficient access)
- Virtual Machine: An Omarchy instance running locally on your Mac

In **Manage Machines**, **Remove** stops the selected VM and deletes its disk files. If that VM was active, it also clears the current selection.

## On the Horizon

- Windows Support
- File Shares
- Audio / Video streaming
- Broader Window Manager (Wayland)
