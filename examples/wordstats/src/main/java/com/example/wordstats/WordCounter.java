package com.example.wordstats;

import com.example.wordstats.report.WordCount;
import com.google.common.base.Splitter;
import com.google.common.collect.HashMultiset;
import com.google.common.collect.ImmutableList;
import com.google.common.collect.ImmutableSet;
import com.google.common.collect.Multiset;
import com.google.common.io.Resources;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.util.Comparator;
import java.util.List;
import java.util.Locale;
import java.util.Set;
import org.apache.commons.lang3.StringUtils;

/** Accumulates case- and accent-insensitive word counts, ignoring stop words. */
public final class WordCounter {
    private static final Splitter WORDS = Splitter.onPattern("[^\\p{L}\\p{N}']+").omitEmptyStrings();

    private final Set<String> stopWords;
    private final Multiset<String> counts = HashMultiset.create();

    public WordCounter(Set<String> stopWords) {
        this.stopWords = ImmutableSet.copyOf(stopWords);
    }

    /** A counter using the English stop-word list bundled as a resource. */
    public static WordCounter withDefaultStopWords() throws IOException {
        String list = Resources.toString(
                Resources.getResource(WordCounter.class, "/wordstats/stopwords.txt"), StandardCharsets.UTF_8);
        return new WordCounter(ImmutableSet.copyOf(
                Splitter.on('\n').trimResults().omitEmptyStrings().split(list)));
    }

    public void accept(String text) {
        for (String raw : WORDS.split(StringUtils.stripAccents(text).toLowerCase(Locale.ROOT))) {
            String word = StringUtils.strip(raw, "'");
            if (!word.isEmpty() && !stopWords.contains(word)) {
                counts.add(word);
            }
        }
    }

    public int totalWords() {
        return counts.size();
    }

    public int distinctWords() {
        return counts.elementSet().size();
    }

    /** The {@code n} most frequent words; ties are broken alphabetically so output is stable. */
    public List<WordCount> top(int n) {
        return counts.entrySet().stream()
                .sorted(Comparator.<Multiset.Entry<String>>comparingInt(Multiset.Entry::getCount)
                        .reversed()
                        .thenComparing(Multiset.Entry::getElement))
                .limit(n)
                .map(e -> new WordCount(e.getElement(), e.getCount()))
                .collect(ImmutableList.toImmutableList());
    }
}
