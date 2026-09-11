package proguard;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.util.ArrayList;
import java.util.List;

/**
 * A stand-in for ProGuard, so jrs's tests can drive the whole obfuscation
 * pipeline without the real 20 MB tool. See {@link fake.Fake} for the compiler
 * equivalent.
 *
 * <p>jrs hands it a single {@code @config} argument — the {@code java} launcher
 * leaves it alone, since it follows the main class. It reads that configuration
 * file, records every line beside the output jar for the test to check, and
 * copies {@code -injars} to {@code -outjars} so an obfuscated jar exists. It
 * renames nothing: the test asserts the configuration, not the bytecode.
 */
public final class ProGuard {
    private ProGuard() {}

    public static void main(String[] rawArgs) throws IOException {
        List<String> config = new ArrayList<>();
        for (String arg : rawArgs) {
            if (arg.startsWith("@")) {
                config.addAll(Files.readAllLines(Path.of(arg.substring(1))));
            } else {
                config.add(arg);
            }
        }
        Path injar = null;
        Path outjar = null;
        for (String raw : config) {
            String line = raw.trim();
            if (line.startsWith("-injars ")) {
                injar = Path.of(unquote(line.substring("-injars ".length()).trim()));
            } else if (line.startsWith("-outjars ")) {
                outjar = Path.of(unquote(line.substring("-outjars ".length()).trim()));
            }
        }
        if (injar == null || outjar == null) {
            System.err.println("proguard: missing -injars or -outjars");
            System.exit(2);
        }
        Files.copy(injar, outjar, StandardCopyOption.REPLACE_EXISTING);
        // The configuration jrs wrote, plus a marker proving the support jar —
        // the compiler's one transitive dependency — was on the classpath, so
        // the test sees the tool was resolved as a graph, not a lone jar.
        List<String> record = new ArrayList<>(config);
        record.add("support=" + fake.support.Support.name());
        Files.write(outjar.resolveSibling(outjar.getFileName() + ".proguard"), record);
    }

    private static String unquote(String s) {
        if (s.length() >= 2 && s.startsWith("\"") && s.endsWith("\"")) {
            return s.substring(1, s.length() - 1);
        }
        return s;
    }
}
