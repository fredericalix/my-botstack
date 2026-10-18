// SPDX-License-Identifier: MIT
//
// Station Wi-Fi with the networks the board gives: saved_networks() reads the
// StackChan firmware's format (the StackChan's own NVS; the P4's `wifi_nvs`
// partition, written by its USB flash). Wi-Fi settings stay in RAM; the
// password is never logged.
#pragma once

#include <string>
#include <vector>

namespace net {

struct Network {
    std::string ssid;
    std::string password;
    /// Join without encryption. Only a board that allows open networks sets
    /// it; otherwise WPA2 is the minimum.
    bool open = false;
};

/// The networks saved in NVS namespace "wifi" of `partition`, the StackChan
/// firmware's format: ssid/password, then ssid1/password1 ... ssid9/password9.
/// Opened read-only; the partition must already be initialised. The StackChan
/// reads its own "nvs" partition.
std::vector<Network> saved_networks(const char* partition = "nvs");

/// Connect to the first of `networks` that answers. Blocks up to `timeout_ms`.
bool wifi_connect(const std::vector<Network>& networks, int timeout_ms);
bool wifi_connected();
/// Station MAC, lowercase "aa:bb:cc:dd:ee:ff" (the robot's device id); on a
/// chip without its own Wi-Fi (the P4), its base MAC.
std::string mac_address();

}  // namespace net
