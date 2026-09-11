package scala.tools.nsc;

/** The fake Scaladoc 2, which ships in the compiler: see {@link fake.Fake}. */
public final class ScalaDoc {
    private ScalaDoc() {}

    public static void main(String[] args) throws Exception {
        System.exit(fake.Fake.document("scaladoc", args));
    }
}
