package io.as4tainer.jvm;

import java.util.function.Function;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

/**
 * Source fixes before Jariko parses a program.
 *
 * <ul>
 *   <li>IBM's compiler ignores columns 1-5 of fixed-form source (the
 *       sequence number area). Programs kept as stream files often put
 *       documentation comments starting with {@code //} in column 1; Jariko
 *       reads those as code. Such lines are made blank.</li>
 *   <li>{@code /COPY} and {@code /INCLUDE} name a stream file in quotes
 *       ({@code /INCLUDE 'header.rpgleinc'}), which Jariko does not
 *       understand (it takes {@code file,member} only). Those are replaced
 *       with the file's lines here.</li>
 * </ul>
 */
final class SourceForms {
    private SourceForms() {}

    /** A /COPY or /INCLUDE of a quoted stream file name. */
    private static final Pattern QUOTED_INCLUDE =
            Pattern.compile("^.{0,6}?\\s*/(?:COPY|INCLUDE)\\s+'([^']+)'.*$", Pattern.CASE_INSENSITIVE);

    static String normalize(String source) {
        return normalize(source, path -> null);
    }

    /** @param include the text of a stream file by path, or null when there is none */
    static String normalize(String source, Function<String, String> include) {
        return normalize(source, include, 0);
    }

    private static String normalize(String source, Function<String, String> include, int depth) {
        boolean free = isFullyFree(source);
        StringBuilder out = new StringBuilder(source.length());
        for (String line : source.split("\n", -1)) {
            String l = line.endsWith("\r") ? line.substring(0, line.length() - 1) : line;
            if (!free && l.startsWith("//")) l = "";
            Matcher m = QUOTED_INCLUDE.matcher(l);
            if (m.matches()) {
                String text = depth < 16 ? include.apply(m.group(1)) : null;
                if (text == null) {
                    throw new IllegalStateException("The /COPY or /INCLUDE file " + m.group(1)
                            + " was not found.");
                }
                String body = normalize(text, include, depth + 1);
                if (body.endsWith("\n")) body = body.substring(0, body.length() - 1);
                out.append(body).append('\n');
                continue;
            }
            out.append(l).append('\n');
        }
        return out.toString();
    }

    /** {@code **FREE} in column 1 of the first line: fully free-form source. */
    static boolean isFullyFree(String source) {
        String first = source.stripLeading();
        int nl = first.indexOf('\n');
        String line = (nl < 0 ? first : first.substring(0, nl)).trim();
        return line.equalsIgnoreCase("**FREE");
    }
}
