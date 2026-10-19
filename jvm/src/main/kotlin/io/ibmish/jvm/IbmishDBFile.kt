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
 *
 * Files the program opens for update lock the records they read (see
 * [RecordLocks]): a read with the N extender does not lock, and a read
 * whose lock wait fails comes back with the error indicator on when the
 * program has one (or the E extender); otherwise the program ends with the
 * error, as on IBM i.
 */
class IbmishDBFile(
    override var name: String,
    override var fileMetadata: FileMetadata,
    private val client: BridgeClient,
) : DBFile {
    override var logger: Logger? = null

    private val handle: Int = client.open(name, RecordLocks.isUpdate(name))

    init {
        val format = client.metadata(name)["format"] as? String
        RecordLocks.register(name, format) { client.unlock(handle) }
    }

    /** Run a read with the flags the program announced for it. */
    private fun locked(chain: Boolean = false, op: (Boolean) -> ReadResult): Result {
        val flags = RecordLocks.takeFlags(name)
        return try {
            result(op(!flags.contains('N')), chain)
        } catch (e: io.ibmish.bridge.BridgeException) {
            if (flags.contains('E') && (e.message ?: "").startsWith("CPF5027")) {
                lastEof = false
                Result(indicatorLO = true)
            } else {
                throw e
            }
        }
    }
    private var lastEof = false
    private var lastEqual = false

    override fun eof(): Boolean = lastEof

    override fun equal(): Boolean = lastEqual

    private fun result(r: ReadResult, chain: Boolean): Result {
        lastEof = r.eof
        val rec = Record()
        r.record.forEach { (k, v) -> rec.add(RecordField(k, v)) }
        // An empty record tells Jariko "not found". The resulting
        // indicators (RPG reference): CHAIN sets the one in positions 71-72
        // (HI) when no record is found; READ, READE, READP and READPE set
        // the one in positions 75-76 (EQ) at end of file.
        return Result(
            record = rec,
            indicatorHI = chain && !r.found,
            indicatorEQ = !chain && r.eof,
        )
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

    override fun chain(keys: List<String>): Result = locked(chain = true) { client.chain(handle, keys, it) }

    override fun read(): Result = locked { client.read(handle, it) }

    override fun readPrevious(): Result = locked { client.readPrevious(handle, it) }

    override fun readEqual(): Result = locked { client.readEqual(handle, null, it) }

    override fun readEqual(key: String): Result = readEqual(listOf(key))

    override fun readEqual(keys: List<String>): Result = locked { client.readEqual(handle, keys, it) }

    override fun readPreviousEqual(): Result = locked { client.readPreviousEqual(handle, null, it) }

    override fun readPreviousEqual(key: String): Result = readPreviousEqual(listOf(key))

    override fun readPreviousEqual(keys: List<String>): Result = locked { client.readPreviousEqual(handle, keys, it) }

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
