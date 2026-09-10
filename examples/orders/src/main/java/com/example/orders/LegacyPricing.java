package com.example.orders;

/**
 * Pricing rules from before the Kotlin rewrite, still in Java: a bulk discount
 * and money formatting. The Kotlin code calls them, and {@link #receipt} reads
 * a Kotlin {@link Order} back.
 */
public final class LegacyPricing {
    /** Orders of at least this many units get {@link #BULK_DISCOUNT_PERCENT} off. */
    public static final int BULK_UNITS = 10;

    public static final int BULK_DISCOUNT_PERCENT = 5;

    private LegacyPricing() {}

    /** The discount, in cents, on a subtotal for an order of {@code units} units. */
    public static long discountCents(int units, long subtotalCents) {
        return units >= BULK_UNITS ? subtotalCents * BULK_DISCOUNT_PERCENT / 100 : 0;
    }

    /** {@code 1205} as {@code 12.05}. */
    public static String format(long cents) {
        return String.format("%d.%02d", cents / 100, cents % 100);
    }

    /** The receipt's closing lines. */
    public static String receipt(Order order, long subtotalCents) {
        long discount = discountCents(order.getUnits(), subtotalCents);
        StringBuilder out = new StringBuilder()
                .append(order.getUnits())
                .append(" units in ")
                .append(order.getItems().size())
                .append(" lines")
                .append(System.lineSeparator());
        if (discount > 0) {
            out.append("bulk discount -").append(format(discount)).append(System.lineSeparator());
        }
        return out.append("total ").append(format(subtotalCents - discount)).toString();
    }
}
