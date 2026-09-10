package org.jetbrains.kotlin.cli.jvm;

/** The fake kotlinc: see {@link fake.Fake}. */
public final class K2JVMCompiler {
    private K2JVMCompiler() {}

    public static void main(String[] args) throws Exception {
        System.exit(fake.Fake.compile("kotlinc", args, ".kt", false));
    }
}
