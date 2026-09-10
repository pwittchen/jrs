package com.example.calc

/**
 * The calculator as Java sees it: a string in, a number out. A top-level
 * object gets a static forwarder, so Java calls `Calculator.evaluate(...)`.
 */
object Calculator:
  def evaluate(text: String): Double = Parser.parse(text).eval
