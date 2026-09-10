package org.codehaus.groovy.tools;

/**
 * The fake groovyc: see {@link fake.Fake}. Groovy's compiler lives in its
 * runtime jar, so this jar is published as both.
 */
public final class FileSystemCompiler {
    private FileSystemCompiler() {}

    public static void main(String[] args) throws Exception {
        boolean joint = java.util.Arrays.asList(args).contains("-j");
        System.exit(fake.Fake.compile("groovyc", args, ".groovy", joint));
    }
}
