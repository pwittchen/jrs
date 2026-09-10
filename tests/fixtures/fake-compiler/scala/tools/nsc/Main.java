package scala.tools.nsc;

/** The fake Scala 2 compiler: see {@link fake.Fake}. */
public final class Main {
    private Main() {}

    public static void main(String[] args) throws Exception {
        System.exit(fake.Fake.compile("scalac", args, ".scala", false));
    }
}
