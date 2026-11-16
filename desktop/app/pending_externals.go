package app

// rc2fx_installer::helper (crates/rc2fx-installer/src/helper.rs), ported as
// package installer/helper. These names stood in for it until it existed;
// each now forwards to exactly one helper name, so the call sites in
// package app did not change. They can be inlined at the call sites at any
// time.

import "github.com/ibanks42/rc2fx/desktop/installer/helper"

// installerUSBPermission is helper.USBPermission (helper::USB_PERMISSION):
// the start of the one failure the udev rule fixes.
const installerUSBPermission = helper.USBPermission

// installerUSBRulePath is helper.USBRulePath (helper::USB_RULE_PATH).
const installerUSBRulePath = helper.USBRulePath

// installerUSBRuleFlag is helper.USBRuleFlag (helper::USB_RULE_FLAG).
const installerUSBRuleFlag = helper.USBRuleFlag

// installerHelperFlag is helper.Flag (helper::FLAG).
const installerHelperFlag = helper.Flag

// installerUSBRule is helper.USBRule (helper::USB_RULE, the bytes of
// config/udev/70-rc2fx.rules, which the helper package embeds).
var installerUSBRule = helper.USBRule

// installerNeeded is helper.Needed() (helper::needed): whether installing
// asks for the system's password.
func installerNeeded() bool { return helper.Needed() }

// installerRequestUSBRule is helper.RequestUSBRule() (helper::request_usb_rule):
// runs `pkexec <app> --install-usb-rule` and waits.
func installerRequestUSBRule() error { return helper.RequestUSBRule() }

// installerInstallUSBRule is helper.InstallUSBRule() (helper::install_usb_rule):
// the privileged side, started through pkexec.
func installerInstallUSBRule() error { return helper.InstallUSBRule() }

// installerServe is helper.Serve(root, work) (helper::serve): the
// privileged installer process, JSON lines on stdio.
func installerServe(root, work string) error { return helper.Serve(root, work) }
