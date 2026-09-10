module Test.Smoke where

import Prelude
import Effect (Effect)
import Effect.Aff (launchAff_, delay, forkAff, joinFiber)
import Effect.Class (liftEffect)
import Effect.Console (log)
import Effect.Ref as Ref
import Data.Time.Duration (Milliseconds(..))
import Test.Assert (assertEqual)

main :: Effect Unit
main = launchAff_ do
  a <- pure 42
  b <- pure (a + 1)
  c <- pure (b + 1)
  liftEffect $ assertEqual { actual: c, expected: 44 }
  ref <- liftEffect $ Ref.new 0
  fiber <- forkAff do
    delay (Milliseconds 10.0)
    liftEffect $ Ref.modify (_ + 1) ref
  first <- joinFiber fiber
  second <- joinFiber fiber
  result <- liftEffect $ Ref.read ref
  liftEffect do
    assertEqual { actual: first, expected: 1 }
    assertEqual { actual: second, expected: 1 }
    assertEqual { actual: result, expected: 1 }
    log "[OK] Aff smoke: pure/bind, delayed fork, shared Ref and repeated join"
