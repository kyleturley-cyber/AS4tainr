package io.as4tainr.jvm;

import com.smeup.dbnative.ConnectionConfig;
import com.smeup.dbnative.DBManagerBaseImpl;
import com.smeup.dbnative.file.DBFile;
import com.smeup.dbnative.model.FileMetadata;
import io.as4tainr.bridge.BridgeClient;

import java.io.IOException;
import java.util.HashMap;
import java.util.Map;

/**
 * The reload DBMManager for display record formats declared as DISK files by
 * {@link Workstation#preprocess}. Configured with one ConnectionConfig per
 * record format, {@code url = as4tainr-dspf://HOST:PORT}. reload creates it
 * reflectively with the ConnectionConfig.
 */
public final class DisplayDBMManager extends DBManagerBaseImpl<String, Object> {
    public static final String URL_PREFIX = "as4tainr-dspf://";

    private final ConnectionConfig connectionConfig;
    private BridgeClient client;
    private final Map<String, DisplayRecordFile> open = new HashMap<>();

    public DisplayDBMManager(ConnectionConfig connectionConfig) {
        this.connectionConfig = connectionConfig;
    }

    @Override
    public ConnectionConfig getConnectionConfig() {
        return connectionConfig;
    }

    private synchronized BridgeClient client() {
        if (client == null) {
            try {
                client = BridgeClient.connect(connectionConfig.getUrl().substring(URL_PREFIX.length()));
            } catch (IOException e) {
                throw new IllegalStateException("cannot reach as4tainr core: " + e.getMessage(), e);
            }
        }
        return client;
    }

    @Override
    public void validateConfig() {
        if (!connectionConfig.getUrl().startsWith(URL_PREFIX)) {
            throw new IllegalArgumentException("display file URLs look like " + URL_PREFIX + "HOST:PORT");
        }
    }

    @Override
    public FileMetadata metadataOf(String name) {
        return Runner.Displays.reloadMetadata(name);
    }

    @Override
    public boolean existFile(String name) {
        return Workstation.format(name) != null;
    }

    @Override
    public synchronized DBFile openFile(String name) {
        Workstation.Format f = Workstation.format(name);
        if (f == null) throw new IllegalArgumentException(name + " is not a display record format");
        return open.computeIfAbsent(f.name, n -> new DisplayRecordFile(f, metadataOf(n), client()));
    }

    @Override
    public synchronized void closeFile(String name) {
        open.remove(name.toUpperCase());
    }

    @Override
    public synchronized void close() {
        open.clear();
        if (client != null) {
            try {
                client.close();
            } catch (IOException ignored) {
                // closing anyway
            }
            client = null;
        }
    }
}
