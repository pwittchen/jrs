package org.codehaus.groovy.tools.groovydoc;

/** The fake Groovydoc: see {@link fake.Fake}. */
public final class Main {
    private Main() {}

    public static void main(String[] args) throws Exception {
        System.exit(fake.Fake.document("groovydoc", args));
    }
}
