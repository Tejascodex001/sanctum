package io.sanctum.sanctum

import android.annotation.SuppressLint
import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothManager
import android.bluetooth.BluetoothServerSocket
import android.bluetooth.BluetoothSocket
import android.bluetooth.le.AdvertiseCallback
import android.bluetooth.le.AdvertiseData
import android.bluetooth.le.AdvertiseSettings
import android.bluetooth.le.ScanCallback
import android.bluetooth.le.ScanFilter
import android.bluetooth.le.ScanResult
import android.bluetooth.le.ScanSettings
import android.content.Context
import android.os.Build
import android.os.SystemClock
import androidx.annotation.RequiresApi
import java.io.Closeable
import java.io.DataInputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.net.InetAddress
import java.net.ServerSocket
import java.net.Socket
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong
import kotlin.concurrent.thread

/**
 * Nearby over Bluetooth: friends with no Wi-Fi and no internet.
 *
 * The phone advertises (Bluetooth LE) only a rotating 8-byte ID that just
 * friends can recognize (the Rust side computes and checks it), plus the
 * port of its link listener. Links are LE L2CAP channels (Android 10+),
 * unpaired: the Rust side runs its own signed handshake and MLS over them.
 *
 * Bytes are relayed through localhost so the Rust node treats a Bluetooth
 * link like any other connection: incoming links go to Rust's nearby
 * listener; for outgoing ones Rust connects to [start]'s relay port, sends
 * the 8-byte handle of a device seen in a scan, and gets a link to it.
 */
@SuppressLint("MissingPermission") // requested before start, see MainActivity
@RequiresApi(Build.VERSION_CODES.Q)
class BluetoothNearby(context: Context, private val onSeen: (ByteArray, Long) -> Unit) {
    private val adapter: BluetoothAdapter? = context.getSystemService(BluetoothManager::class.java)?.adapter
    private var server: BluetoothServerSocket? = null
    private var relay: ServerSocket? = null
    private var advertising: AdvertiseCallback? = null
    private var scanning: ScanCallback? = null
    private val devices = ConcurrentHashMap<Long, Pair<BluetoothDevice, Int>>()
    private val handles = ConcurrentHashMap<String, Long>()
    private val lastReported = ConcurrentHashMap<String, Long>()
    private val nextHandle = AtomicLong(1)

    @Volatile
    private var running = false
    private var psm = 0

    /** Starts listening and scanning; returns the relay port, or -1. */
    fun start(rustPort: Int): Int {
        val adapter = adapter ?: return -1
        if (!adapter.isEnabled) return -1
        stop()
        running = true

        val server = adapter.listenUsingInsecureL2capChannel()
        this.server = server
        psm = server.psm
        thread(name = "sanctum-bt-accept", isDaemon = true) {
            while (running) {
                val link = try {
                    server.accept()
                } catch (e: IOException) {
                    break
                }
                thread(isDaemon = true) {
                    try {
                        pipe(link, Socket(InetAddress.getLoopbackAddress(), rustPort))
                    } catch (e: IOException) {
                        quietly(link)
                    }
                }
            }
        }

        val relay = ServerSocket(0, 16, InetAddress.getLoopbackAddress())
        this.relay = relay
        thread(name = "sanctum-bt-relay", isDaemon = true) {
            while (running) {
                val local = try {
                    relay.accept()
                } catch (e: IOException) {
                    break
                }
                thread(isDaemon = true) { dial(local) }
            }
        }

        scan()
        return relay.localPort
    }

    /** Advertises this minute's ID (called again as it rotates). */
    fun advertise(id: ByteArray) {
        val advertiser = adapter?.bluetoothLeAdvertiser ?: return
        if (!running) return
        advertising?.let { runCatching { advertiser.stopAdvertising(it) } }
        val payload = byteArrayOf(MAGIC, VERSION, (psm and 0xFF).toByte(), ((psm shr 8) and 0xFF).toByte()) + id
        val settings = AdvertiseSettings.Builder()
            .setAdvertiseMode(AdvertiseSettings.ADVERTISE_MODE_BALANCED)
            .setTxPowerLevel(AdvertiseSettings.ADVERTISE_TX_POWER_MEDIUM)
            .setConnectable(true)
            .setTimeout(0)
            .build()
        val data = AdvertiseData.Builder()
            .setIncludeDeviceName(false)
            .setIncludeTxPowerLevel(false)
            .addManufacturerData(COMPANY, payload)
            .build()
        val callback = object : AdvertiseCallback() {}
        advertising = callback
        runCatching { advertiser.startAdvertising(settings, data, callback) }
    }

    fun stop() {
        running = false
        scanning?.let { cb -> runCatching { adapter?.bluetoothLeScanner?.stopScan(cb) } }
        advertising?.let { cb -> runCatching { adapter?.bluetoothLeAdvertiser?.stopAdvertising(cb) } }
        scanning = null
        advertising = null
        quietly(server)
        quietly(relay)
        server = null
        relay = null
    }

    private fun scan() {
        val scanner = adapter?.bluetoothLeScanner ?: return
        val filter = ScanFilter.Builder()
            .setManufacturerData(COMPANY, byteArrayOf(MAGIC, VERSION), byteArrayOf(-1, -1))
            .build()
        val settings = ScanSettings.Builder().setScanMode(ScanSettings.SCAN_MODE_BALANCED).build()
        val callback = object : ScanCallback() {
            override fun onScanResult(callbackType: Int, result: ScanResult) = seen(result)

            override fun onBatchScanResults(results: MutableList<ScanResult>) = results.forEach { seen(it) }
        }
        scanning = callback
        runCatching { scanner.startScan(listOf(filter), settings, callback) }
    }

    private fun seen(result: ScanResult) {
        val data = result.scanRecord?.getManufacturerSpecificData(COMPANY) ?: return
        if (data.size < 4 + ID_LEN || data[0] != MAGIC || data[1] != VERSION) return
        val psm = (data[2].toInt() and 0xFF) or ((data[3].toInt() and 0xFF) shl 8)
        val id = data.copyOfRange(4, 4 + ID_LEN)
        val address = result.device.address
        // Adverts repeat many times a second; report each one now and then.
        val key = address + id.contentToString()
        val now = SystemClock.elapsedRealtime()
        if (now - (lastReported[key] ?: 0L) < REPORT_EVERY_MS) return
        lastReported[key] = now
        val handle = handles.getOrPut(address) { nextHandle.getAndIncrement() }
        devices[handle] = result.device to psm
        onSeen(id, handle)
    }

    /** Rust asked for a link: read the device handle, connect, relay. */
    private fun dial(local: Socket) {
        try {
            val bytes = ByteArray(8)
            DataInputStream(local.getInputStream()).readFully(bytes)
            val handle = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN).long
            val (device, psm) = devices[handle] ?: throw IOException("unknown device")
            val link = device.createInsecureL2capChannel(psm)
            link.connect()
            pipe(link, local)
        } catch (e: IOException) {
            quietly(local)
        }
    }

    /** Relays bytes both ways until either side closes. */
    private fun pipe(link: BluetoothSocket, local: Socket) {
        local.tcpNoDelay = true
        thread(isDaemon = true) {
            copy(link.inputStream, local.getOutputStream())
            quietly(local)
            quietly(link)
        }
        copy(local.getInputStream(), link.outputStream)
        quietly(local)
        quietly(link)
    }

    private fun copy(input: InputStream, output: OutputStream) {
        val buffer = ByteArray(16 * 1024)
        try {
            while (true) {
                val n = input.read(buffer)
                if (n < 0) break
                output.write(buffer, 0, n)
                output.flush()
            }
        } catch (e: IOException) {
            // The other side closed.
        }
    }

    private fun quietly(c: Closeable?) {
        runCatching { c?.close() }
    }

    companion object {
        /** The Bluetooth SIG's company ID reserved for testing; nothing official is ours. */
        const val COMPANY = 0xFFFF
        const val MAGIC: Byte = 0x53 // 'S'
        const val VERSION: Byte = 1
        const val ID_LEN = 8
        const val REPORT_EVERY_MS = 8_000L
    }
}
