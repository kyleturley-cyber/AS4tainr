package io.as4tainr.jvm;

import com.smeup.rpgparser.interpreter.FeaturesFactory;
import java.io.PrintStream;

/**
 * Entry point of as4tainr-jvm.jar.
 *
 * Jariko prints a version banner and its feature-flag status on standard output
 * the first time it starts. Standard output belongs to the RPG program (DSPLY
 * output, and what golden-output tests compare), so the banner is sent to
 * standard error by initializing Jariko while System.out points there.
 */
public final class Launcher {
    private Launcher() {}

    public static void main(String[] args) {
        PrintStream out = System.out;
        System.setOut(System.err);
        try {
            FeaturesFactory.INSTANCE.newInstance();
        } finally {
            System.setOut(out);
        }
        Runner.main(args);
    }
}
