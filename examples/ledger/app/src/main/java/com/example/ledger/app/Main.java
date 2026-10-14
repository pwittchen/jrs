package com.example.ledger.app;

import com.example.ledger.journal.Journal;
import java.io.IOException;
import java.io.InputStreamReader;
import java.io.Reader;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Objects;

/** Prints the balances of a journal: the one named on the command line, or a sample. */
public final class Main {

    private Main() {}

    public static void main(String[] args) throws IOException {
        try (Reader reader = args.length > 0
                ? Files.newBufferedReader(Path.of(args[0]), StandardCharsets.UTF_8)
                : new InputStreamReader(
                        Objects.requireNonNull(Main.class.getResourceAsStream("/sample.csv"), "sample.csv"),
                        StandardCharsets.UTF_8)) {
            System.out.print(Report.render(Journal.read(reader)));
        } catch (Journal.JournalException e) {
            System.err.println("ledger: " + e.getMessage());
            System.exit(1);
        }
    }
}
