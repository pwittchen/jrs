package com.example.wordstats;

import com.example.wordstats.report.Report;
import com.example.wordstats.report.ReportFormat;
import com.google.common.io.Resources;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;

/** Counts word frequencies in the given files, or in a bundled sample text. */
public final class Main {
    private static final String USAGE = String.join("\n",
            "usage: wordstats [--top N] [--format NAME] [FILE...]",
            "",
            "Prints the most frequent words in FILE, or in a bundled sample text",
            "when no file is given. Formats: " + String.join(", ", ReportFormat.available()) + ".");

    private Main() {}

    public static void main(String[] args) throws IOException {
        int top = 10;
        String format = "text";
        List<Path> files = new ArrayList<>();

        try {
            for (int i = 0; i < args.length; i++) {
                switch (args[i]) {
                    case "-h", "--help" -> {
                        System.out.println(USAGE);
                        return;
                    }
                    case "--top" -> top = Integer.parseInt(value(args, ++i, "--top"));
                    case "--format" -> format = value(args, ++i, "--format");
                    default -> files.add(Path.of(args[i]));
                }
            }
            if (top < 1) {
                throw new IllegalArgumentException("--top must be at least 1");
            }
        } catch (IllegalArgumentException e) {
            System.err.println("wordstats: " + e.getMessage());
            System.err.println(USAGE);
            System.exit(2);
            return;
        }

        ReportFormat renderer = ReportFormat.named(format);
        WordCounter counter = WordCounter.withDefaultStopWords();
        String source;
        if (files.isEmpty()) {
            source = "bundled sample";
            counter.accept(Resources.toString(
                    Resources.getResource(Main.class, "/wordstats/sample.txt"), StandardCharsets.UTF_8));
        } else {
            source = String.join(", ", files.stream().map(Path::toString).toList());
            for (Path file : files) {
                counter.accept(Files.readString(file, StandardCharsets.UTF_8));
            }
        }

        System.out.print(renderer.render(new Report(
                source, counter.totalWords(), counter.distinctWords(), counter.top(top))));
    }

    private static String value(String[] args, int index, String flag) {
        if (index >= args.length) {
            throw new IllegalArgumentException(flag + " needs a value");
        }
        return args[index];
    }
}
