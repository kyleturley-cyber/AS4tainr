package io.ibmish.jvm

import com.smeup.dbnative.file.DBFile
import com.smeup.dbnative.file.Record
import com.smeup.dbnative.file.RecordField
import com.smeup.dbnative.file.Result
import com.smeup.dbnative.log.Logger
import com.smeup.dbnative.model.FileMetadata
import io.ibmish.bridge.BridgeClient
import io.ibmish.bridge.ReadResult

/**
 * One open file (one F-spec) in a Jariko program. Each operation is one
 * bridge round trip; %EOF and %EQUAL are cached from the last response.
 */
class IbmishDBFile(
    override var name: String,
    override var fileMetadata: FileMetadata,
    private val client: BridgeClient,
) : DBFile {
    override var logger: Logger? = null

    private val handle: Int = client.open(name)
    private var lastEof = false
    private var lastEqual = false

    override fun eof(): Boolean = lastEof

    override fun equal(): Boolean = lastEqual

    private fun result(r: ReadResult): Result {
        lastEof = r.eof
        val rec = Record()
        r.record.forEach { (k, v) -> rec.add(RecordField(k, v)) }
        // An empty record tells Jariko "not found".
        return Result(record = rec)
    }

    override fun setll(key: String): Boolean = setll(listOf(key))

    override fun setll(keys: List<String>): Boolean {
        lastEof = false
        lastEqual = client.setll(handle, keys)
        return lastEqual
    }

    override fun setgt(key: String): Boolean = setgt(listOf(key))

    override fun setgt(keys: List<String>): Boolean {
        lastEof = false
        lastEqual = false
        client.setgt(handle, keys)
        return true
    }

    override fun chain(key: String): Result = chain(listOf(key))

    override fun chain(keys: List<String>): Result = result(client.chain(handle, keys))

    override fun read(): Result = result(client.read(handle))

    override fun readPrevious(): Result = result(client.readPrevious(handle))

    override fun readEqual(): Result = result(client.readEqual(handle, null))

    override fun readEqual(key: String): Result = readEqual(listOf(key))

    override fun readEqual(keys: List<String>): Result = result(client.readEqual(handle, keys))

    override fun readPreviousEqual(): Result = result(client.readPreviousEqual(handle, null))

    override fun readPreviousEqual(key: String): Result = readPreviousEqual(listOf(key))

    override fun readPreviousEqual(keys: List<String>): Result = result(client.readPreviousEqual(handle, keys))

    override fun write(record: Record): Result {
        client.write(handle, record)
        return Result(record = record)
    }

    override fun update(record: Record): Result {
        client.update(handle, record)
        return Result(record = record)
    }

    override fun delete(record: Record): Result {
        client.delete(handle)
        return Result(record = record)
    }

    override fun close() {
        try {
            client.closeFile(handle)
        } catch (e: Exception) {
            // The bridge may already be gone at job end.
        }
    }
}
