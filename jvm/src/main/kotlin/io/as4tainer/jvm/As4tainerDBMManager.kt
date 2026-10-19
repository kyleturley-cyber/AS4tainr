package io.as4tainer.jvm

import com.smeup.dbnative.ConnectionConfig
import com.smeup.dbnative.DBManagerBaseImpl
import com.smeup.dbnative.file.DBFile
import com.smeup.dbnative.model.Field
import com.smeup.dbnative.model.FileMetadata
import io.as4tainer.bridge.BridgeClient

/**
 * A reload DBMManager that sends every file operation to as4tainer core over
 * the bridge, instead of talking to a database over JDBC.
 *
 * Configure it with `impl = io.as4tainer.jvm.As4tainerDBMManager` and
 * `url = as4tainer://HOST:PORT`. reload instantiates it reflectively with a
 * single ConnectionConfig argument.
 */
class As4tainerDBMManager(
    override val connectionConfig: ConnectionConfig,
) : DBManagerBaseImpl<String, Any>() {
    private val client: BridgeClient by lazy { BridgeClient.connect(hostPort()) }
    private val openFiles = mutableMapOf<String, As4tainerDBFile>()

    private fun hostPort(): String = connectionConfig.url.removePrefix(URL_PREFIX)

    override fun validateConfig() {
        require(connectionConfig.url.startsWith(URL_PREFIX)) {
            "as4tainer connection URLs look like ${URL_PREFIX}HOST:PORT, got ${connectionConfig.url}"
        }
    }

    override fun metadataOf(name: String): FileMetadata = reloadMetadata(client.metadata(name))

    override fun existFile(name: String): Boolean =
        try {
            client.metadata(name)
            true
        } catch (e: io.as4tainer.bridge.BridgeException) {
            false
        }

    override fun openFile(name: String): DBFile =
        openFiles.getOrPut(name.uppercase()) {
            As4tainerDBFile(name.uppercase(), metadataOf(name), client)
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
        const val URL_PREFIX = "as4tainer://"

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
