package org.jetbrains.kotlin.cli.jvm;

import java.io.PrintStream;

/**
 * The fake kotlinc: see {@link fake.Fake}. Like the real one it reads its
 * arguments from an {@code @argfile}, and it has the real one's
 * {@code exec(PrintStream, String...)}, which jrs's {@code --watch} worker
 * calls in process.
 */
public final class K2JVMCompiler {
    public K2JVMCompiler() {}

    public static void main(String[] args) throws Exception {
        System.exit(run(args));
    }

    public Code exec(PrintStream err, String... args) throws Exception {
        return new Code(run(args));
    }

    private static int run(String[] args) throws Exception {
        return fake.Fake.compile("kotlinc", fake.Fake.expand(args), ".kt", false);
    }

    /** The real compiler's {@code ExitCode}, as far as jrs reads it. */
    public static final class Code {
        private final int code;

        Code(int code) {
            this.code = code;
        }

        public int getCode() {
            return code;
        }
    }
}
