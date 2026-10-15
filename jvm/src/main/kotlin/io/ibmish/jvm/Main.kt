package io.ibmish.jvm

import com.smeup.dbnative.ConnectionConfig
import com.smeup.dbnative.DBNativeAccessConfig
import com.smeup.rpgparser.execution.Configuration
import com.smeup.rpgparser.execution.ReloadConfig
import com.smeup.rpgparser.execution.executePgmWithStringArgs
import com.smeup.rpgparser.interpreter.DbField
import com.smeup.rpgparser.interpreter.NumberType
import com.smeup.rpgparser.interpreter.StringType
import com.smeup.rpgparser.parsing.parsetreetoast.RpgType
import com.smeup.rpgparser.rpginterop.DirRpgProgramFinder
import io.ibmish.bridge.BridgeClient
import java.io.File
import kotlin.system.exitProcess
import com.smeup.rpgparser.interpreter.FileMetadata as JarikoFileMetadata

/**
 * The ibmish RPG runner. ibmish core launches it as:
 *
 *     java -jar ibmish-jvm.jar --bridge HOST:PORT PGM [parm...]
 *
 * Programs are looked up as PGM.rpgle in the directories listed in
 * IBMISH_PGM_PATH (path-separator separated; default ./qrpglesrc).
 * All file I/O goes back to ibmish core through the bridge.
 */
fun main(args: Array<String>) {
    if (args.size < 3 || args[0] != "--bridge") {
        System.err.println("usage: ibmish-jvm --bridge HOST:PORT PGM [parm...]")
        exitProcess(2)
    }
    val hostPort = args[1]
    val program = args[2].uppercase()
    val parms = args.drop(3)

    val dirs =
        (System.getenv("IBMISH_PGM_PATH") ?: "qrpglesrc")
            .split(File.pathSeparator)
            .map(::File)
            .filter { it.isDirectory }
    if (dirs.isEmpty()) {
        System.err.println("CPF4101: no program directory found (set IBMISH_PGM_PATH)")
        exitProcess(1)
    }

    // A separate connection for metadata lookups at compile time.
    val metadataClient = BridgeClient.connect(hostPort)

    val configuration = Configuration()
    configuration.reloadConfig =
        ReloadConfig(
            nativeAccessConfig =
                DBNativeAccessConfig(
                    listOf(
                        ConnectionConfig(
                            fileName = "*",
                            url = IbmishDBMManager.URL_PREFIX + hostPort,
                            user = "",
                            password = "",
                            driver = null,
                            impl = IbmishDBMManager::class.java.name,
                        ),
                    ),
                ),
            metadataProducer = { dbFile -> jarikoMetadata(metadataClient.metadata(dbFile)) },
        )

    try {
        executePgmWithStringArgs(
            programName = program,
            programArgs = parms,
            programFinders = dirs.map { DirRpgProgramFinder(it) },
            configuration = configuration,
        )
    } catch (e: Throwable) {
        System.err.println("RNQ0202: program $program ended with an error: ${e.message}")
        exitProcess(1)
    } finally {
        metadataClient.close()
    }
}

/** Map an ibmish file description to Jariko's typed FileMetadata. */
@Suppress("UNCHECKED_CAST")
fun jarikoMetadata(md: Map<String, Any?>): JarikoFileMetadata {
    val fields =
        (md["fields"] as List<Map<String, Any?>>).map {
            val name = it["name"] as String
            val length = (it["length"] as Number).toInt()
            val decimals = (it["decimals"] as Number).toInt()
            val type =
                when (it["type"]) {
                    "P" -> NumberType(length - decimals, decimals, RpgType.PACKED)
                    "S" -> NumberType(length - decimals, decimals, RpgType.ZONED)
                    else -> StringType(length, false)
                }
            DbField(name, type)
        }
    return JarikoFileMetadata(
        name = md["name"] as String,
        tableName = "${md["library"]}/${md["name"]}",
        recordFormat = md["format"] as String,
        fields = fields,
        accessFields = md["keys"] as List<String>,
    )
}
