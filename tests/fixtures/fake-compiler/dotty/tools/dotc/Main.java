package dotty.tools.dotc;

/** The fake Scala 3 compiler: see {@link fake.Fake}. */
public final class Main {
    private Main() {}

    public static void main(String[] args) throws Exception {
        System.exit(fake.Fake.compile("scalac", args, ".scala", false));
    }
}
