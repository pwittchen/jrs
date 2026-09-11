package dotty.tools.scaladoc;

/** The fake Scala 3 scaladoc: see {@link fake.Fake}. */
public final class Main {
    private Main() {}

    public static void main(String[] args) throws Exception {
        System.exit(fake.Fake.document("scaladoc", args));
    }
}
