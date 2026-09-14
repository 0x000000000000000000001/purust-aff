module Test.NativeIO where

import Prelude
import Effect (Effect)
import Effect.Aff (launchAff_)
import Effect.Class (liftEffect)
import Effect.Console (log)

foreign import start :: Effect Unit

main :: Effect Unit
main = launchAff_ do
  liftEffect start
  liftEffect $ log "[OK] native IO parent returned"
