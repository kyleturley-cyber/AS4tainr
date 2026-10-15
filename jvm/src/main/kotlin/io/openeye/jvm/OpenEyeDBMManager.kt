package io.openeye.jvm

import com.smeup.dbnative.ConnectionConfig
import com.smeup.dbnative.DBManagerBaseImpl
import com.smeup.dbnative.file.DBFile
import com.smeup.dbnative.model.Field
import com.smeup.dbnative.model.FileMetadata
import io.openeye.bridge.BridgeClient

/**
 * A reload DBMManager that sends every file operation to OpenEye core over
 * the bridge, instead of talking to a database over JDBC.
 *
 * Configure it with `impl = io.openeye.jvm.OpenEyeDBMManager` and
 * `url = openeye://HOST:PORT`. reload instantiates it reflectively with a
 * single ConnectionConfig argument.
 */
class OpenEyeDBMManager(
    override val connectionConfig: ConnectionConfig,
) : DBManagerBaseImpl<String, Any>() {
    private val client: BridgeClient by lazy { BridgeClient.connect(hostPort()) }
    private val openFiles = mutableMapOf<String, OpenEyeDBFile>()

    private fun hostPort(): String = connectionConfig.url.removePrefix(URL_PREFIX)

    override fun validateConfig() {
        require(connectionConfig.url.startsWith(URL_PREFIX)) {
            "OpenEye connection URLs look like ${URL_PREFIX}HOST:PORT, got ${connectionConfig.url}"
        }
    }

    override fun metadataOf(name: String): FileMetadata = reloadMetadata(client.metadata(name))

    override fun existFile(name: String): Boolean =
        try {
            client.metadata(name)
            true
        } catch (e: io.openeye.bridge.BridgeException) {
            false
        }

    override fun openFile(name: String): DBFile =
        openFiles.getOrPut(name.uppercase()) {
            OpenEyeDBFile(name.uppercase(), metadataOf(name), client)
        }

    override fun closeFile(name: String) {
        openFiles.remove(name.uppercase())?.close()
    }

    override fun close() {
        openFiles.values.forEach { it.close() }
        openFiles.clear()
        client.close()
    }

    companion object {
        const val URL_PREFIX = "openeye://"

        @Suppress("UNCHECKED_CAST")
        fun reloadMetadata(md: Map<String, Any?>): FileMetadata {
            val fields = (md["fields"] as List<Map<String, Any?>>).map {
                Field(it["name"] as String, (it["text"] as? String) ?: "", it["numeric"] == true)
            }
            return FileMetadata(
                md["name"] as String,
                "${md["library"]}/${md["name"]}",
                fields,
                md["keys"] as List<String>,
            )
        }
    }
}
