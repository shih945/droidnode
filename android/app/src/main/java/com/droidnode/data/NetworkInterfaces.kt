package com.droidnode.data

import android.content.Context
import java.io.File
import java.net.Inet4Address
import java.net.NetworkInterface

/**
 * Reads/writes the small ASCII files the agent consumes for network selection
 * (`network-preference.env`) and exposes the device's IPv4 interfaces so the
 * settings page can let the operator pick one.
 */
object NetworkInterfaces {

    private const val NETWORK_PREF_ENV = "network-preference.env"
    private const val KEY_NODE_IFACE = "DROIDNODE_NODE_IFACE"

    fun preferenceFile(ctx: Context): File = File(ctx.filesDir, NETWORK_PREF_ENV)

    /** Empty means "auto"; the agent reads this at startup. */
    fun nodeIface(ctx: Context): String = try {
        val file = preferenceFile(ctx)
        if (file.exists()) {
            file.readLines()
                .map { it.trim() }
                .firstOrNull { it.startsWith("$KEY_NODE_IFACE=") }
                ?.substringAfter('=')
                ?.trim()
                .orEmpty()
        } else {
            ""
        }
    } catch (e: Exception) {
        ""
    }

    fun setNodeIface(ctx: Context, iface: String): Boolean = try {
        val file = preferenceFile(ctx)
        if (iface.isBlank()) {
            file.delete()
        } else {
            ctx.filesDir.mkdirs()
            // ASCII only: the file is parsed by the Rust side.
            val body = "# written by Settings; read by the agent at startup\n" +
                "$KEY_NODE_IFACE=${iface.trim()}\n"
            file.writeText(body, Charsets.US_ASCII)
        }
        true
    } catch (e: Exception) {
        false
    }

    /** Device IPv4 interfaces (name -> address) for the network picker. */
    fun listIpv4(): List<Pair<String, String>> = try {
        NetworkInterface.getNetworkInterfaces()
            .toList()
            .filter { it.isUp && !it.isLoopback }
            .flatMap { nif ->
                nif.inetAddresses.toList()
                    .filterIsInstance<Inet4Address>()
                    .map { nif.name to it.hostAddress.orEmpty() }
            }
            .filter { it.second.isNotEmpty() }
            .distinctBy { it.second }
    } catch (e: Exception) {
        emptyList()
    }

    /** Wi-Fi Direct / USB tethering / tunnels: private IPs the cluster cannot route to. */
    fun isTetherLike(name: String): Boolean {
        val n = name.lowercase()
        return listOf("p2p", "rndis", "usb", "tun", "tap").any { n.startsWith(it) }
    }
}
